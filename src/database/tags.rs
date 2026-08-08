/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Tag persistence: tag CRUD, bookmark↔tag links, and tag statistics.
//!
//! Tags are a flat list (`tags`) joined to bookmarks through `bookmark_tags`
//! (many-to-many, cascading deletes on both sides). This module owns every
//! SQL statement that touches those two tables — the CLI and HTTP layers
//! call these functions instead of writing their own queries, so tag
//! behavior is identical across both front doors.
//!
//! Link semantics worth knowing:
//! * `set_bookmark_tags` is a destructive full replace (delete + re-insert).
//! * `add_bookmark_tags` / `remove_bookmark_tags` are incremental.
//! * Blank tag names are skipped, never created — an empty tag is
//!   meaningless and would pollute the taxonomy.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{OrphanTag, TagCount};

/// Returns the id of `name`, creating the tag row on first use.
///
/// Atomic upsert (no check-then-insert race): `INSERT ... ON CONFLICT DO
/// NOTHING` then a plain SELECT. The `ON CONFLICT` target is the unique
/// `tags.name` column, so concurrent inserts can't double-create.
pub fn get_or_create(conn: &Connection, name: &str) -> Result<i64> {
	// Same atomic upsert trick as `categories::get_or_create`.
	conn.execute(
		"INSERT INTO tags (name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
		params![name],
	)
	.context("failed to ensure tag exists")?;
	let id = conn.query_row(
		"SELECT id FROM tags WHERE name = ?1",
		params![name],
		|row| row.get(0),
	)?;
	crate::log_trace!("tag {name:?} resolved to id #{id}");
	Ok(id)
}

/// All tags with their active-bookmark counts, most-used first (ties broken
/// alphabetically). The join filters on `trashed_at IS NULL`, so trashed
/// bookmarks don't inflate a tag's count. `limit` is `None` for the full
/// list (CLI, `/api/tags`); `Some` pages it (overview's top 5, `/api/stats/tags`).
pub fn list_with_counts(
	conn: &Connection,
	limit: Option<usize>,
	offset: usize,
) -> Result<Vec<TagCount>> {
	// `limit`/`offset` are validated upstream (1..=1000, offset >= 0), so
	// interpolating them is safe.
	let page = match limit {
		Some(l) => format!(" LIMIT {l} OFFSET {offset}"),
		None => String::new(),
	};
	let sql = format!(
		"SELECT t.name, COUNT(bt.bookmark_id) as cnt
         FROM tags t
         LEFT JOIN bookmark_tags bt ON bt.tag_id = t.id
         LEFT JOIN bookmarks b ON b.id = bt.bookmark_id AND b.trashed_at IS NULL
         GROUP BY t.id
         ORDER BY cnt DESC, t.name ASC{page}"
	);
	let mut stmt = conn.prepare(&sql)?;
	let rows = stmt.query_map([], |row| {
		Ok(TagCount {
			name: row.get(0)?,
			count: row.get(1)?,
		})
	})?;
	Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Renames a tag. Returns `false` when no such tag exists (so callers can
/// distinguish "renamed" from "was already gone"). Blank names are rejected
/// before touching the database.
pub fn rename(conn: &Connection, old_name: &str, new_name: &str) -> Result<bool> {
	let new_name = new_name.trim();
	if new_name.is_empty() {
		anyhow::bail!("tag name cannot be empty");
	}
	let rows = conn.execute(
		"UPDATE tags SET name = ?1 WHERE name = ?2",
		params![new_name, old_name],
	)?;
	let renamed = rows > 0;
	crate::log_trace!("rename tag {old_name:?} -> {new_name:?}: {renamed}");
	Ok(renamed)
}

/// Deletes a tag. Returns `false` when no such tag exists. The junction
/// rows are cleaned up by the `ON DELETE CASCADE` on `bookmark_tags.tag_id`
/// — no explicit second DELETE needed.
pub fn delete(conn: &Connection, name: &str) -> Result<bool> {
	// Deleting the tag row is enough — `ON DELETE CASCADE` on
	// `bookmark_tags.tag_id` clears the link rows for us.
	let rows = conn.execute("DELETE FROM tags WHERE name = ?1", params![name])?;
	let deleted = rows > 0;
	crate::log_trace!("delete tag {name:?}: {deleted}");
	Ok(deleted)
}

/// Full replace: delete then re-insert. Simpler than diffing, and the
/// per-tag ON CONFLICT DO NOTHING keeps this safe to rerun.
///
/// Used for the `tags` field of an update (`tags: [..]`), which the API
/// documents as "replace the whole set". Blank entries are dropped.
pub fn set_bookmark_tags(conn: &Connection, bookmark_id: i64, tags: &[String]) -> Result<()> {
	conn.execute(
		"DELETE FROM bookmark_tags WHERE bookmark_id = ?1",
		params![bookmark_id],
	)?;
	for tag in tags {
		let tag = tag.trim();
		if tag.is_empty() {
			continue;
		}
		let tag_id = get_or_create(conn, tag)?;
		conn.execute(
			"INSERT OR IGNORE INTO bookmark_tags (bookmark_id, tag_id) VALUES (?1, ?2)",
			params![bookmark_id, tag_id],
		)?;
	}
	crate::log_trace!("set {} tags on bookmark #{bookmark_id}", tags.len());
	Ok(())
}

/// Additive link update: creates each named tag if needed and links it,
/// leaving the bookmark's existing tags untouched. `INSERT OR IGNORE`
/// makes re-adding an existing link a harmless no-op.
pub fn add_bookmark_tags(conn: &Connection, bookmark_id: i64, tags: &[String]) -> Result<()> {
	for tag in tags {
		let tag = tag.trim();
		if tag.is_empty() {
			continue;
		}
		let tag_id = get_or_create(conn, tag)?;
		conn.execute(
			"INSERT OR IGNORE INTO bookmark_tags (bookmark_id, tag_id) VALUES (?1, ?2)",
			params![bookmark_id, tag_id],
		)?;
	}
	crate::log_trace!("added {} tags to bookmark #{bookmark_id}", tags.len());
	Ok(())
}

/// Subtractive link update: unlinks each named tag. Tags that don't exist,
/// or links that don't exist, are silently ignored (removing an absent tag
/// is not an error).
pub fn remove_bookmark_tags(conn: &Connection, bookmark_id: i64, tags: &[String]) -> Result<()> {
	for tag in tags {
		let tag = tag.trim();
		if tag.is_empty() {
			continue;
		}
		// Resolve the id first so a nonexistent tag can't error the DELETE.
		let tag_id_opt: Option<i64> = conn
			.query_row("SELECT id FROM tags WHERE name = ?1", params![tag], |row| {
				row.get(0)
			})
			.optional()?;
		if let Some(tag_id) = tag_id_opt {
			conn.execute(
				"DELETE FROM bookmark_tags WHERE bookmark_id = ?1 AND tag_id = ?2",
				params![bookmark_id, tag_id],
			)?;
		}
	}
	crate::log_trace!("removed {} tags from bookmark #{bookmark_id}", tags.len());
	Ok(())
}

/// The tag names attached to one bookmark, alphabetical — flattened into
/// the `Bookmark.tags` field when a bookmark is read back.
pub fn get_bookmark_tags(conn: &Connection, bookmark_id: i64) -> Result<Vec<String>> {
	let mut stmt = conn.prepare(
		"SELECT t.name FROM tags t
         JOIN bookmark_tags bt ON bt.tag_id = t.id
         WHERE bt.bookmark_id = ?1
         ORDER BY t.name",
	)?;
	let rows = stmt.query_map(params![bookmark_id], |row| row.get::<_, String>(0))?;
	Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Tags attached to exactly one active bookmark — candidates for cleanup.
///
/// "Orphan" here means *under-used*, not dangling: the `GROUP BY t.id` +
/// `HAVING COUNT = 1` picks tags with a single active owner, and the join
/// reports which bookmark that is. Trashed bookmarks don't count as owners.
/// `limit`/`offset` page the list.
pub fn orphan_tags(conn: &Connection, limit: usize, offset: usize) -> Result<Vec<OrphanTag>> {
	let mut stmt = conn.prepare(
		"SELECT t.name, b.id, b.title
         FROM tags t
         JOIN bookmark_tags bt ON bt.tag_id = t.id
         JOIN bookmarks b ON b.id = bt.bookmark_id AND b.trashed_at IS NULL
         GROUP BY t.id
         HAVING COUNT(bt.bookmark_id) = 1
         ORDER BY t.name ASC
         LIMIT ?1 OFFSET ?2",
	)?;
	let rows = stmt.query_map(params![limit as i64, offset as i64], |row| {
		Ok(OrphanTag {
			name: row.get(0)?,
			bookmark_id: row.get(1)?,
			bookmark_title: row.get(2)?,
		})
	})?;
	let tags = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("orphan tags -> {} rows", tags.len());
	Ok(tags)
}
