/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Bookmark persistence: the full CRUD surface, filters, FTS search, the
//! recycle bin, and duplicate detection.
//!
//! This is the largest module in the database layer and the heart of the
//! app. The HTTP layer is its only caller, so a behavior fixed here (a
//! filter, a search rule, a duplicate message) is identical across every
//! endpoint.
//!
//! # Design notes
//!
//! * **One query shape per concern** — `list`/`count` build their WHERE
//!   clauses from the same `BookmarkFilter` and MUST stay in lockstep (the
//!   HTTP `x-total-count` header is computed by `count` and must equal the
//!   length of `list`'s result).
//! * **Duplicates are friendly, not raw** — `insert`/`update` pre-check the
//!   URL and keyword and bail with a clear message; `http::error`
//!   special-cases those messages into the 409 `conflict_url` /
//!   `conflict_keyword` contracts.
//! * **Trash is a column, not a state machine** — `trashed_at IS NULL`
//!   means active; every read path filters on it, and FTS triggers keep the
//!   search indexes quarantined for free.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::HashMap;

use crate::core::media;
use crate::model::{Bookmark, BookmarkFilter, BulkRemoveResult, NewBookmark, UpdateBookmark};
use crate::shared::{MAX_PAGE_SIZE, extract_domain};

use super::categories::get_or_create;
use super::tags::{add_bookmark_tags, get_bookmark_tags, remove_bookmark_tags, set_bookmark_tags};

/// Hard ceiling for unpaginated reads (export, keywords) — a safety cap,
/// not a user-facing page size.
const MAX_UNPAGINATED: i64 = 100_000;

/// The columns every bookmark read selects, aliased so `row_to_bookmark`
/// can address them by name. `category_name` comes from the LEFT JOIN; the
/// tag names are *not* here because they're one-to-many and filled in by
/// `attach_tags` after the row query.
const SELECT_BOOKMARK_FIELDS: &str = "
    b.id, b.title, b.url, b.description, b.domain, b.category_id, c.name as category_name,
    b.starred, b.keyword, b.note, b.favicon, b.thumbnail, b.visit_count,
    b.last_visited_at, b.is_archived, b.created_at, b.updated_at, b.trashed_at
";

/// Maps one SQL row onto a `Bookmark`, addressing columns by name (so the
/// field list above can't get out of sync with the struct). SQLite stores
/// booleans as integers, hence the `!= 0` conversions for `starred` and
/// `is_archived`.
fn row_to_bookmark(row: &Row) -> rusqlite::Result<Bookmark> {
	Ok(Bookmark {
		id: row.get("id")?,
		title: row.get("title")?,
		url: row.get("url")?,
		description: row.get("description")?,
		domain: row.get("domain")?,
		category_id: row.get("category_id")?,
		category_name: row.get("category_name")?,
		starred: row.get::<_, i64>("starred")? != 0,
		keyword: row.get("keyword")?,
		note: row.get("note")?,
		favicon: row.get("favicon")?,
		thumbnail: row.get("thumbnail")?,
		visit_count: row.get("visit_count")?,
		last_visited_at: row.get("last_visited_at")?,
		is_archived: row.get::<_, i64>("is_archived")? != 0,
		created_at: row.get("created_at")?,
		updated_at: row.get("updated_at")?,
		trashed_at: row.get("trashed_at")?,
		tags: Vec::new(), // filled in by attach_tags()
	})
}

/// Fills the (one-to-many) `tags` field on every bookmark in a result set
/// with a single batched query (`IN (...)`) grouped in memory — one query
/// per page instead of one per bookmark. The global `ORDER BY t.name` keeps
/// each bookmark's tag list alphabetical, matching `get_bookmark_tags`.
fn attach_tags(conn: &Connection, mut bookmarks: Vec<Bookmark>) -> Result<Vec<Bookmark>> {
	if bookmarks.is_empty() {
		return Ok(bookmarks);
	}
	let ids: Vec<i64> = bookmarks.iter().map(|b| b.id).collect();
	let placeholders = vec!["?"; ids.len()].join(",");
	let mut stmt = conn.prepare(&format!(
		"SELECT bt.bookmark_id, t.name FROM bookmark_tags bt
		 JOIN tags t ON t.id = bt.tag_id
		 WHERE bt.bookmark_id IN ({placeholders})
		 ORDER BY t.name"
	))?;
	let mut rows = stmt.query(rusqlite::params_from_iter(ids))?;
	let mut tags_by_bookmark: HashMap<i64, Vec<String>> = HashMap::new();
	while let Some(row) = rows.next()? {
		tags_by_bookmark
			.entry(row.get::<_, i64>(0)?)
			.or_default()
			.push(row.get::<_, String>(1)?);
	}
	for b in &mut bookmarks {
		b.tags = tags_by_bookmark.remove(&b.id).unwrap_or_default();
	}
	Ok(bookmarks)
}

// ============================================================
// Create / read / update / delete
// ============================================================

/// Creates a bookmark. `NewBookmark` has defaults for everything except
/// `url`: a blank title falls back to the URL, a blank/missing category
/// falls back to the default, `starred` defaults to false, and a missing
/// keyword stays unset. Returns the new row id.
///
/// Duplicate URL/keyword handling is a friendly pre-check *before* the
/// INSERT — see the note at the top of this module. Media resolution (and
/// its bundled-default-token collision guard) runs through
/// `core::media::resolve_new`.
/// Looks up the active bookmark that already owns `url` (if any), excluding
/// `exclude_id` so a row being updated can't collide with itself. Shared by
/// the friendly pre-checks and the post-violation message re-derivation.
/// Callers with no row to exclude pass `-1` (ids start at 1).
pub(crate) fn find_url_owner(
	conn: &Connection,
	url: &str,
	exclude_id: i64,
) -> Result<Option<(i64, String)>> {
	conn.query_row(
		"SELECT id, title FROM bookmarks
		 WHERE url = ?1 AND trashed_at IS NULL AND id != ?2",
		params![url, exclude_id],
		|row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
	)
	.optional()
	.map_err(anyhow::Error::from)
}

/// Looks up the active bookmark that already owns `keyword`
/// (case-insensitively, consistent with `get_by_keyword`), excluding
/// `exclude_id`. `ORDER BY id LIMIT 1` keeps the result deterministic if
/// mixed-case duplicates ever exist from a legacy database.
pub(crate) fn find_keyword_owner(
	conn: &Connection,
	keyword: &str,
	exclude_id: i64,
) -> Result<Option<(i64, String)>> {
	conn.query_row(
		"SELECT id, title FROM bookmarks
		 WHERE keyword = ?1 COLLATE NOCASE AND trashed_at IS NULL AND id != ?2
		 ORDER BY id LIMIT 1",
		params![keyword, exclude_id],
		|row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
	)
	.optional()
	.map_err(anyhow::Error::from)
}

/// True when `err` is a SQLITE_CONSTRAINT_UNIQUE failure (extended result
/// code 2067) — the raw form a duplicate URL/keyword INSERT or UPDATE
/// produces when the friendly pre-checks lose their race to a concurrent
/// writer.
pub(crate) fn is_unique_violation(err: &rusqlite::Error) -> bool {
	matches!(
		err,
		rusqlite::Error::SqliteFailure(
			rusqlite::ffi::Error {
				extended_code: 2067,
				..
			},
			_
		)
	)
}

/// Friendly message for a UNIQUE violation: whichever pre-check now hits
/// names the owner, so the HTTP layer sees the same message the pre-check
/// would have produced. (If the row vanished again in the race, fall back
/// to a generic phrasing rather than a raw constraint string.)
pub(crate) fn duplicate_error(
	conn: &Connection,
	url: &str,
	keyword: Option<&str>,
	exclude_id: i64,
) -> anyhow::Error {
	match find_url_owner(conn, url, exclude_id) {
		Ok(Some((id, title))) => anyhow::anyhow!("URL already exists as bookmark #{id} ({title})"),
		_ => match keyword.and_then(|k| find_keyword_owner(conn, k, exclude_id).ok().flatten()) {
			Some((id, title)) => anyhow::anyhow!(
				"keyword \"{}\" already in use by bookmark #{id} ({title})",
				keyword.unwrap_or_default()
