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
			),
			None => anyhow::anyhow!("a bookmark with this URL or keyword already exists"),
		},
	}
}

pub fn insert(conn: &Connection, new: &NewBookmark) -> Result<i64> {
	// Media resolution (the precedence logic + any network fetch) lives in
	// `core::media`, shared with the HTTP layer. The HTTP handler resolves
	// first — on a separate blocking task, without holding the writer — and
	// calls `insert_resolved`; `insert` is the convenience wrapper that
	// resolves then inserts.
	let media = media::resolve_new(new).map_err(anyhow::Error::msg)?;
	insert_resolved(conn, new, media)
}

/// Runs the friendly duplicate pre-checks for a *new* bookmark (URL, then
/// keyword). Split out so the HTTP handler can reject a duplicate request
/// *before* resolving media — a colliding save must not trigger a needless
/// network fetch or cache write.
pub fn check_insert_collisions(conn: &Connection, new: &NewBookmark) -> Result<()> {
	// An empty string and "no keyword" are the same thing on creation —
	// if we inserted "" literally, a second bookmark with no keyword would
	// fail the partial unique index on keyword (unlike NULL, two empty
	// strings do collide under a UNIQUE index).
	let keyword = new.keyword.clone().filter(|k| !k.is_empty());

	// Friendly duplicate detection: check before the INSERT so the user
	// gets a clear message instead of a raw UNIQUE-constraint error.
	// Only *active* rows collide — a trashed bookmark with the same URL or
	// keyword never blocks re-adding it (partial unique indexes).
	if let Some((existing_id, existing_title)) = find_url_owner(conn, &new.url, -1)? {
		anyhow::bail!("URL already exists as bookmark #{existing_id} ({existing_title})");
	}
	if let Some(keyword) = keyword.as_deref()
		&& let Some((existing_id, existing_title)) = find_keyword_owner(conn, keyword, -1)?
	{
		anyhow::bail!(
			"keyword \"{keyword}\" already in use by bookmark #{existing_id} ({existing_title})"
		);
	}
	Ok(())
}

/// Persistence-only insert: the caller has already resolved media via
/// `core::media::resolve_new` (so the HTTP layer never holds the writer
/// lock across a network fetch). `insert` wraps this with the resolution.
pub fn insert_resolved(
	conn: &Connection,
	new: &NewBookmark,
	media: media::ResolvedMedia,
) -> Result<i64> {
	// Blank/missing category → default. `filter` distinguishes "not sent"
	// from "sent but blank"; both mean the default category.
	let category_name = new
		.category
		.as_deref()
		.filter(|c| !c.trim().is_empty())
		.unwrap_or(crate::model::DEFAULT_CATEGORY);
	let category_id = get_or_create(conn, category_name)?;
	let domain = extract_domain(&new.url);
	check_insert_collisions(conn, new)?;

	let title = new
		.title
		.clone()
		.filter(|t| !t.trim().is_empty())
		.unwrap_or_else(|| new.url.clone());

	if let Err(err) = conn.execute(
		"INSERT INTO bookmarks
            (title, url, description, domain, category_id, starred, keyword, note,
             favicon, thumbnail, is_archived)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
		params![
			title,
			new.url,
			new.description,
			domain,
			category_id,
			new.starred.unwrap_or(false),
			new.keyword.clone().filter(|k| !k.is_empty()),
			new.note,
			media.favicon,
			media.thumbnail,
			new.is_archived.unwrap_or(false),
		],
	) {
		// A concurrent writer can slip a duplicate past the pre-checks;
		// the UNIQUE index catches it and must surface as the same friendly
		// message, not a raw constraint string (the HTTP layer would
		// otherwise classify the 2067 into a generic 500).
		if is_unique_violation(&err) {
			return Err(duplicate_error(conn, &new.url, keyword(new).as_deref(), -1));
		}
		return Err(err).context("failed to insert bookmark");
	}

	let id = conn.last_insert_rowid();

	// A `tags` request on creation is a full replacement of an empty set —
	// `set_bookmark_tags` deletes (nothing) then inserts the given list.
	if let Some(tags) = &new.tags {
		set_bookmark_tags(conn, id, tags)?;
	}

	crate::log_trace!("inserted bookmark #{id} ({title:?}, domain {domain:?})");
	Ok(id)
}

/// The keyword tri-state used by `insert`-family duplicate handling: an
/// empty string and "no keyword" are the same thing on creation.
fn keyword(new: &NewBookmark) -> Option<String> {
	new.keyword.clone().filter(|k| !k.is_empty())
}

/// Fetches one active bookmark by id. Trashed bookmarks are invisible to
/// this read — the recycle bin has its own query (`list` with `trash=true`).
pub fn get(conn: &Connection, id: i64) -> Result<Option<Bookmark>> {
	let sql = format!(
		"SELECT {SELECT_BOOKMARK_FIELDS}
         FROM bookmarks b LEFT JOIN categories c ON c.id = b.category_id
         WHERE b.id = ?1 AND b.trashed_at IS NULL"
	);
	let bookmark = conn
		.query_row(&sql, params![id], row_to_bookmark)
		.optional()?;
	match bookmark {
		Some(mut b) => {
			b.tags = get_bookmark_tags(conn, b.id)?;
			crate::log_trace!("fetched bookmark #{id} ({:?})", b.title);
			Ok(Some(b))
		}
		None => {
			crate::log_trace!("bookmark #{id} not found or trashed");
			Ok(None)
		}
	}
}

/// Fetches the active bookmark that owns a keyword shortcut — the lookup
/// behind `/keywords/{keyword}`.
///
/// Matching is case-insensitive (`II` / `Ii` / `ii` are the same shortcut):
/// keywords are typed into a browser address bar, and the NOCASE collation
/// is exactly the ASCII fold the `is_valid_keyword` charset guarantees.
/// `ORDER BY id LIMIT 1` is a deterministic tiebreak for pre-existing
/// mixed-case rows that the old BINARY unique index let coexist.
pub fn get_by_keyword(conn: &Connection, keyword: &str) -> Result<Option<Bookmark>> {
	let sql = format!(
		"SELECT {SELECT_BOOKMARK_FIELDS}
         FROM bookmarks b LEFT JOIN categories c ON c.id = b.category_id
         WHERE b.keyword = ?1 COLLATE NOCASE AND b.trashed_at IS NULL
         ORDER BY b.id ASC LIMIT 1"
	);
	let bookmark = conn
		.query_row(&sql, params![keyword], row_to_bookmark)
		.optional()?;
	match bookmark {
		Some(mut b) => {
			b.tags = get_bookmark_tags(conn, b.id)?;
			crate::log_trace!("keyword {keyword:?} -> bookmark #{}", b.id);
			Ok(Some(b))
		}
		None => {
			crate::log_trace!("keyword {keyword:?} matches no active bookmark");
			Ok(None)
		}
	}
}

/// Builds the shared filter → SQL mapping for a `BookmarkFilter`: the
/// `WHERE` conditions and their bound values, against the aliases used by
/// the bookmark selects (`b` for bookmarks, `c` for categories).
///
/// This is the single source of truth for "what does this filter match",
/// consumed by `list`, `count`, `select_ids`, and `remove_matching`. They
/// MUST stay in lockstep — the HTTP `x-total-count` header is `count`'s
/// result and must equal the length of `list`'s array, and a bulk delete
/// must touch exactly the rows a dry-run preview showed.
