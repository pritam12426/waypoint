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
    b.starred, b.keyword, b.redirect_template, b.note, b.favicon,
    b.thumbnail, b.visit_count, b.last_visited_at, b.is_archived, b.created_at,
    b.updated_at, b.trashed_at
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
		redirect_template: row.get("redirect_template")?,
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
		Ok(Some((id, title))) => anyhow::anyhow!(
			"URL already exists as bookmark #{id} ({title}) — you can't save the same URL \
			 twice; open bookmark #{id} to view or edit it"
		),
		_ => match keyword.and_then(|k| find_keyword_owner(conn, k, exclude_id).ok().flatten()) {
			Some((id, title)) => anyhow::anyhow!(
				"keyword \"{}\" already in use by bookmark #{id} ({title}) — pick a different \
				 keyword or leave it empty",
				keyword.unwrap_or_default()
			),
			None => anyhow::anyhow!(
				"a bookmark with this URL or keyword already exists (a concurrent save may \
				 have just landed); trashed copies don't count — restore or purge the \
				 conflicting one first"
			),
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
		anyhow::bail!(
			"URL already exists as bookmark #{existing_id} ({existing_title}) — you can't \
			 save the same URL twice; open bookmark #{existing_id} to view or edit it"
		);
	}
	if let Some(keyword) = keyword.as_deref()
		&& let Some((existing_id, existing_title)) = find_keyword_owner(conn, keyword, -1)?
	{
		anyhow::bail!(
			"keyword \"{keyword}\" already in use by bookmark #{existing_id} ({existing_title}) \
			 — pick a different keyword or leave it empty"
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
             favicon, thumbnail, is_archived, redirect_template)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
			new.redirect_template.clone().filter(|t| !t.is_empty()),
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
fn build_where(filter: &BookmarkFilter) -> (Vec<String>, Vec<Box<dyn rusqlite::ToSql>>) {
	let mut conditions: Vec<String> = Vec::new();
	let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

	if filter.trash {
		// Recycle bin: only trashed bookmarks, no archived/active split.
		conditions.push("b.trashed_at IS NOT NULL".into());
	} else {
		match filter.archived {
			Some(true) => conditions.push("b.trashed_at IS NULL AND b.is_archived = 1".into()),
			Some(false) => conditions.push("b.trashed_at IS NULL AND b.is_archived = 0".into()),
			None => conditions.push("b.trashed_at IS NULL".into()),
		}
	}

	if let Some(cat) = &filter.category {
		conditions.push("c.name = ?".into());
		values.push(Box::new(cat.clone()));
	}
	if let Some(category_id) = filter.category_id {
		conditions.push("b.category_id = ?".into());
		values.push(Box::new(category_id));
	}
	if let Some(starred) = filter.starred {
		conditions.push("b.starred = ?".into());
		values.push(Box::new(starred));
	}
	if let Some(tag) = &filter.tag {
		conditions.push(
			"b.id IN (SELECT bt.bookmark_id FROM bookmark_tags bt \
			  JOIN tags t ON t.id = bt.tag_id WHERE t.name = ?)"
				.into(),
		);
		values.push(Box::new(tag.clone()));
	}
	if let Some(keyword) = &filter.keyword {
		conditions.push("b.keyword = ? COLLATE NOCASE".into());
		values.push(Box::new(keyword.clone()));
	}

	// The six time bounds are already normalized `YYYY-MM-DD HH:MM:SS`
	// UTC strings (`shared::parse_datetime_bound`), so plain `>=`/`<=`
	// comparison is chronological — the fixed-width format sorts by value.
	// A NULL `last_visited_at` (never visited) matches `last_visited_before`
	// (the `IS NULL` disjunct) but never `last_visited_after` (NULL >= bound
	// is false in SQL, which is exactly the semantics we want).
	if let Some(bound) = &filter.created_after {
		conditions.push("b.created_at >= ?".into());
		values.push(Box::new(bound.clone()));
	}
	if let Some(bound) = &filter.created_before {
		conditions.push("b.created_at <= ?".into());
		values.push(Box::new(bound.clone()));
	}
	if let Some(bound) = &filter.updated_after {
		conditions.push("b.updated_at >= ?".into());
		values.push(Box::new(bound.clone()));
	}
	if let Some(bound) = &filter.updated_before {
		conditions.push("b.updated_at <= ?".into());
		values.push(Box::new(bound.clone()));
	}
	if let Some(bound) = &filter.last_visited_after {
		conditions.push("b.last_visited_at >= ?".into());
		values.push(Box::new(bound.clone()));
	}
	if let Some(bound) = &filter.last_visited_before {
		conditions.push("(b.last_visited_at IS NULL OR b.last_visited_at <= ?)".into());
		values.push(Box::new(bound.clone()));
	}
	// `trashed_at` bounds only make sense with `trash: true`; they are what
	// "empty the trash up to this date" maps onto.
	if let Some(bound) = &filter.trashed_after {
		conditions.push("b.trashed_at >= ?".into());
		values.push(Box::new(bound.clone()));
	}
	if let Some(bound) = &filter.trashed_before {
		conditions.push("b.trashed_at <= ?".into());
		values.push(Box::new(bound.clone()));
	}

	(conditions, values)
}

/// Lists bookmarks by filter. Builds a WHERE clause from `BookmarkFilter`
/// via `build_where` and appends ORDER BY + LIMIT/OFFSET. Must stay in
/// lockstep with `count` — except for `before_cursor`, which `list` alone
/// consumes (a cursor describes a *page*, not a filter, so `count` must
/// report the whole-corpus total, not the current page's remainder).
///
/// Ordering: recycle-bin view is most-recently-trashed first; everything
/// else is newest-created first. Tag filtering uses a subquery on the
/// junction table so a bookmark with *any* of the tag's links matches once.
///
/// Cursor (keyset) pagination: `before_cursor` carries the `(created_at,
/// id)` of the last row of the previous page, applied as a row-value bound
/// `(b.created_at, b.id) < (?, ?)` on the same columns the ORDER BY walks.
/// With `idx_bookmarks_created` that's an index range SEARCH (constant time
/// regardless of depth) instead of an OFFSET walk. Offset pagination is
/// still supported and `before_cursor` takes precedence when both are set.
/// Only the active (created_at) ordering supports a cursor — the trash view
/// keeps plain offset pagination.
pub fn list(conn: &Connection, filter: &BookmarkFilter) -> Result<Vec<Bookmark>> {
	let mut sql = format!(
		"SELECT {SELECT_BOOKMARK_FIELDS}
         FROM bookmarks b LEFT JOIN categories c ON c.id = b.category_id"
	);

	let (conditions, mut values) = build_where(filter);
	if !conditions.is_empty() {
		sql.push_str(" WHERE ");
		sql.push_str(&conditions.join(" AND "));
	}

	if filter.trash {
		sql.push_str(" ORDER BY b.trashed_at DESC");
	} else {
		// Cursor bound before the ORDER BY: row-value comparison on the same
		// leading key, so the index SEARCH range and the backwards scan
		// share one index. Values bind positionally after the filter's.
		if let Some((created_at, id)) = &filter.before_cursor {
			if !conditions.is_empty() {
				sql.push_str(" AND ");
			} else {
				sql.push_str(" WHERE ");
			}
			sql.push_str("(b.created_at, b.id) < (?, ?)");
			values.push(Box::new(created_at.clone()));
			values.push(Box::new(*id));
		}
		sql.push_str(" ORDER BY b.created_at DESC");
	}

	// These two are validated integers (never user-controlled strings), so
	// splicing them into the SQL text directly carries no injection risk.
	let limit = filter.limit.unwrap_or(200).clamp(1, MAX_UNPAGINATED);
	sql.push_str(&format!(" LIMIT {limit}"));
	// Cursor and offset are mutually exclusive by construction (the HTTP
	// layer sends one or the other); if both somehow arrive, the cursor wins
	// — OFFSET on top of a keyset bound would double-skip.
	if let Some(offset) = filter.offset.filter(|_| filter.before_cursor.is_none()) {
		sql.push_str(&format!(" OFFSET {}", offset.max(0)));
	}

	let mut stmt = conn.prepare(&sql)?;
	// `Box<dyn ToSql>` values are heterogeneous (String vs bool), so they're
	// re-borrowed as `&dyn ToSql` for the bound-parameter list.
	let param_refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|b| b.as_ref()).collect();
	let rows = stmt.query_map(param_refs.as_slice(), row_to_bookmark)?;

	let bookmarks = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("list_bookmarks: {} rows (limit {limit})", bookmarks.len());
	attach_tags(conn, bookmarks)
}

/// Mirrors `list` exactly — same tables, same WHERE conditions (both via
/// `build_where`) — so the HTTP API can report an exact `X-Total-Count`.
/// Keep the two in lockstep.
///
/// If one of them gains a WHERE clause the other lacks, the header silently
/// drifts from the real array length; the HTTP tests would catch it, but
/// the discipline lives here.
pub fn count(conn: &Connection, filter: &BookmarkFilter) -> Result<i64> {
	let mut sql = String::from(
		"SELECT COUNT(*) FROM bookmarks b LEFT JOIN categories c ON c.id = b.category_id",
	);

	let (conditions, values) = build_where(filter);
	if !conditions.is_empty() {
		sql.push_str(" WHERE ");
		sql.push_str(&conditions.join(" AND "));
	}

	let mut stmt = conn.prepare(&sql)?;
	let param_refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|b| b.as_ref()).collect();
	let total = stmt.query_row(param_refs.as_slice(), |row| row.get::<_, i64>(0))?;
	crate::log_trace!("count_bookmarks: {total} matching");
	Ok(total)
}

/// The ids (ascending) of every bookmark a filter matches, unpaginated.
/// This is the basis for criteria-based removal: `remove_matching` acts on
/// exactly what this returns, and a dry-run preview shows the same ids.
pub fn select_ids(conn: &Connection, filter: &BookmarkFilter) -> Result<Vec<i64>> {
	let mut sql =
		String::from("SELECT b.id FROM bookmarks b LEFT JOIN categories c ON c.id = b.category_id");

	let (conditions, values) = build_where(filter);
	if !conditions.is_empty() {
		sql.push_str(" WHERE ");
		sql.push_str(&conditions.join(" AND "));
	}
	sql.push_str(" ORDER BY b.id ASC");

	let mut stmt = conn.prepare(&sql)?;
	let param_refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|b| b.as_ref()).collect();
	let rows = stmt.query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))?;
	let ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("select_ids: {} rows matching", ids.len());
	Ok(ids)
}

/// All active bookmarks, unpaginated — used by export, not by the API
/// `list` endpoint (which goes through `list` and its default page size).
pub fn list_all_active(conn: &Connection) -> Result<Vec<Bookmark>> {
	let filter = BookmarkFilter {
		limit: Some(MAX_UNPAGINATED),
		..Default::default()
	};
	list(conn, &filter)
}

/// Bookmarks that have a keyword shortcut set, ordered by id ascending —
/// used by the `/keywords` route.
///
/// Unlike `list`, the keyword predicate is unconditional (a keyword exists
/// and is non-empty); the archived filter still applies.
pub fn list_keywords(conn: &Connection, filter: &BookmarkFilter) -> Result<Vec<Bookmark>> {
	let mut sql = format!(
		"SELECT {SELECT_BOOKMARK_FIELDS}
         FROM bookmarks b LEFT JOIN categories c ON c.id = b.category_id"
	);

	// Seed with the one condition `list` doesn't have, then reuse the same
	// filter-condition block via `build_where` (archived/category/starred/
	// tag/keyword/time bounds).
	let mut conditions: Vec<String> = vec!["b.keyword IS NOT NULL AND b.keyword != ''".into()];
	let (mut filter_conditions, values) = build_where(filter);
	conditions.append(&mut filter_conditions);

	sql.push_str(" WHERE ");
	sql.push_str(&conditions.join(" AND "));

	sql.push_str(" ORDER BY b.id ASC");

	let limit = filter.limit.unwrap_or(50).clamp(1, MAX_UNPAGINATED);
	sql.push_str(&format!(" LIMIT {limit}"));

	let mut stmt = conn.prepare(&sql)?;
	let param_refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|b| b.as_ref()).collect();
	let rows = stmt.query_map(param_refs.as_slice(), row_to_bookmark)?;

	let bookmarks = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	attach_tags(conn, bookmarks)
}

/// Applies a partial update to an active bookmark. Returns `Some` with the
/// *pre-update* bookmark (the caller uses it for the change log and audit
/// message) or `None` if the id doesn't exist or is trashed.
///
/// The tri-state fields (`keyword`, and the `Option`s generally) mean
/// "not sent" leaves the current value alone, while `Some("")` clears.
///
/// `update` runs the full pipeline: read → collision pre-check → resolve
/// media (`core::media::resolve_update`) → persist. The HTTP handler runs
/// the same pieces but resolves media *first* (on a separate blocking task,
/// without holding the writer) and then calls `update_resolved`.
pub fn update(conn: &Connection, id: i64, update: &UpdateBookmark) -> Result<Option<Bookmark>> {
	let Some(existing) = get(conn, id)? else {
		return Ok(None);
	};
	check_update_collisions(conn, &existing, update)?;
	let media = media::resolve_update(&existing, update).map_err(anyhow::Error::msg)?;
	update_resolved(conn, id, update, &existing, media)
}

/// Friendly duplicate URL/keyword detection for an update, scoped to *other*
/// active rows — re-saving the current URL/keyword (a no-op resend) must not
/// trip it. Runs *before* media resolution so a collision can't trigger a
/// needless network fetch or cache write (the HTTP handler calls it on its
/// read connection for the same reason).
pub fn check_update_collisions(
	conn: &Connection,
	existing: &Bookmark,
	update: &UpdateBookmark,
) -> Result<()> {
	let id = existing.id;
	// "URL changed" means the *value* actually differs from the stored one,
	// not merely that the field was present: the web UI's edit form resends
	// the URL on every save.
	if update.url.as_deref().is_some_and(|u| u != existing.url)
		&& let Some((other_id, other_title)) = find_url_owner(
			conn,
			&update.url.clone().unwrap_or_else(|| existing.url.clone()),
			id,
		)? {
		anyhow::bail!(
			"URL already exists as bookmark #{other_id} ({other_title}) — you can't save the \
			 same URL twice; open bookmark #{other_id} to view or edit it"
		);
	}

	// Tri-state: None = unchanged, Some("") = clear, Some(x) = set.
	let keyword = match &update.keyword {
		Some(k) if k.is_empty() => None,
		Some(k) => Some(k.clone()),
		None => existing.keyword.clone(),
	};
	// NOCASE keeps the uniqueness gate consistent with the lookup: `II` and
	// `ii` are the same shortcut, so a case-variant must not be creatable.
	// The `existing.keyword != kw` guard lets a case-fold of one's own
	// keyword through (the check excludes this row anyway).
	if let Some(kw) = keyword.as_deref()
		&& existing.keyword.as_deref() != Some(kw)
		&& let Some((other_id, other_title)) = find_keyword_owner(conn, kw, id)?
	{
		anyhow::bail!(
			"keyword \"{kw}\" already in use by bookmark #{other_id} ({other_title}) — pick a \
			 different keyword or leave it empty"
		);
	}
	Ok(())
}

/// Persistence half of `update`: re-reads the row, re-runs the collision
/// pre-checks against the *fresh* state, and writes the caller's pre-resolved
/// media. `seen` is the row the caller resolved media against.
///
/// If that row changed in any media-relevant field (url/favicon/thumbnail)
/// between the caller's read and this write — only a concurrent edit of the
/// same bookmark can do that — the passed media is stale, so it is
/// re-resolved against the fresh row. The common path never fetches under
/// the writer lock.
pub fn update_resolved(
	conn: &Connection,
	id: i64,
	update: &UpdateBookmark,
	seen: &Bookmark,
	media: media::ResolvedMedia,
) -> Result<Option<Bookmark>> {
	// Fetch first: we need the current row both to build "unchanged"
	// defaults and to distinguish a real no-op from a missing bookmark.
	let Some(existing) = get(conn, id)? else {
		return Ok(None);
	};
	check_update_collisions(conn, &existing, update)?;

	let media = if existing.url != seen.url
		|| existing.favicon != seen.favicon
		|| existing.thumbnail != seen.thumbnail
	{
		crate::log_trace!(
			"update #{id}: row changed since media resolution (concurrent edit); re-resolving"
		);
		media::resolve_update(&existing, update).map_err(anyhow::Error::msg)?
	} else {
		media
	};

	let title = update
		.title
		.clone()
		.filter(|t| !t.trim().is_empty())
		.unwrap_or(existing.title.clone());
	let url = update.url.clone().unwrap_or(existing.url.clone());
	// Changing the URL re-derives the domain — one update can't leave a
	// stale domain behind.
	let domain = extract_domain(&url);
	// Tri-state: None = unchanged, Some("") = clear, Some(x) = set.
	let keyword = match &update.keyword {
		Some(k) if k.is_empty() => None,
		Some(k) => Some(k.clone()),
		None => existing.keyword.clone(),
	};
	// Same tri-state as `keyword`: empty clears, missing leaves alone.
	let redirect_template = match &update.redirect_template {
		Some(t) if t.is_empty() => None,
		Some(t) => Some(t.clone()),
		None => existing.redirect_template.clone(),
	};
	let description = update.description.clone().or(existing.description.clone());
	let note = update.note.clone().or(existing.note.clone());

	let favicon = media.favicon;
	let thumbnail = media.thumbnail;
	let starred = update.starred.unwrap_or(existing.starred);
	let is_archived = update.is_archived.unwrap_or(existing.is_archived);

	let category_id = match &update.category {
		Some(cat) if !cat.trim().is_empty() => get_or_create(conn, cat)?,
		_ => existing.category_id,
	};

	// A true no-op must leave the row *completely* untouched — including
	// `updated_at`. The `update_bookmark_timestamp` trigger fires on any
	// UPDATE that touches one of its `OF` columns, even when the value is
	// unchanged, so "nothing changed" has to skip the statement entirely.
	// We don't compare `domain`: it's a pure function of `url`, so it can't
	// change when `url` doesn't.
	let changed = title != existing.title
		|| url != existing.url
		|| description != existing.description
		|| note != existing.note
		|| favicon != existing.favicon
		|| thumbnail != existing.thumbnail
		|| starred != existing.starred
		|| is_archived != existing.is_archived
		|| keyword != existing.keyword
		|| redirect_template != existing.redirect_template
		|| category_id != existing.category_id;

	if changed {
		if let Err(err) = conn.execute(
			"UPDATE bookmarks SET
	            title = ?1, url = ?2, description = ?3, domain = ?4, category_id = ?5,
	            starred = ?6, keyword = ?7, note = ?8, favicon = ?9, thumbnail = ?10,
	            is_archived = ?11, redirect_template = ?12,
	            updated_at = CURRENT_TIMESTAMP
	         WHERE id = ?13 AND trashed_at IS NULL",
			params![
				title,
				url,
				description,
				domain,
				category_id,
				starred,
				keyword,
				note,
				favicon,
				thumbnail,
				is_archived,
				redirect_template,
				id,
			],
		) {
			// Same race as insert: a concurrent writer can beat the
			// pre-checks, and the UNIQUE index then fires here.
			if is_unique_violation(&err) {
				return Err(duplicate_error(conn, &url, keyword.as_deref(), id));
			}
			return Err(err).context("failed to update bookmark");
		}
	} else {
		crate::log_trace!("update #{id}: no fields changed, row left untouched");
	}

	// Three independent tag semantics: full replace (`tags`), or additive/
	// subtractive (`add_tags` / `remove_tags`). They can be combined; a
	// replace plus an add would add then still apply. Callers normally
	// send one or the other.
	let tags_before = get_bookmark_tags(conn, id)?;
	if let Some(tags) = &update.tags {
		set_bookmark_tags(conn, id, tags)?;
	}
	if let Some(add) = &update.add_tags
		&& !add.is_empty()
	{
		add_bookmark_tags(conn, id, add)?;
	}
	if let Some(rm) = &update.remove_tags
		&& !rm.is_empty()
	{
		remove_bookmark_tags(conn, id, rm)?;
	}
	// Tag edits live in a separate table, outside the timestamp trigger's
	// `OF` list — bump `updated_at` by hand so a tag-only change still
	// shows as "last modified".
	if get_bookmark_tags(conn, id)? != tags_before {
		conn.execute(
			"UPDATE bookmarks SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
			params![id],
		)?;
	}

	crate::log_trace!("updated bookmark #{id} (is_archived={is_archived}, starred={starred})");
	Ok(Some(existing))
}

/// Trashes one bookmark inside an open transaction, first permanently
/// deleting any *older trashed* copies of its URL. The trash must never hold
/// two bookmarks with the same URL: `remove` → re-add → `remove` would
/// otherwise pile up stale copies (each new removal is a fresh row). The
/// newest trashed copy wins; its predecessors are gone for good.
///
/// The sibling purge only fires for a live target — the subquery requires
/// `trashed_at IS NULL` on `id`, so a stale or already-trashed id purges
/// nothing. Returns whether the target itself was actually trashed.
fn trash_with_dedup(tx: &rusqlite::Transaction, id: i64) -> Result<bool> {
	tx.execute(
		"DELETE FROM bookmarks
		 WHERE url = (SELECT url FROM bookmarks WHERE id = ?1 AND trashed_at IS NULL)
		   AND trashed_at IS NOT NULL AND id != ?1",
		params![id],
	)?;
	let changed = tx.execute(
		"UPDATE bookmarks SET trashed_at = CURRENT_TIMESTAMP
		 WHERE id = ?1 AND trashed_at IS NULL",
		params![id],
	)?;
	Ok(changed > 0)
}

/// Soft path of a delete (`DELETE /api/bookmarks/{id}` without purge):
/// move a bookmark into the trash. Sets `trashed_at`; the FTS trash
/// trigger removes its search entry. Any older trashed copy of the same
/// URL is purged first (see `trash_with_dedup`), so the trash stays free
/// of duplicate URLs.
/// Returns `false` when the bookmark doesn't exist or is already trashed.
pub fn trash(conn: &Connection, id: i64) -> Result<bool> {
	let tx = conn.unchecked_transaction()?;
	let ok = trash_with_dedup(&tx, id)?;
	tx.commit()?;
	crate::log_trace!("trash bookmark #{id}: {ok}");
	Ok(ok)
}

/// Pulls a trashed bookmark back out of the recycle bin (sets `trashed_at`
/// back to NULL). The FTS restore trigger re-adds it to the right index
/// (main or archive, by its `is_archived` state). Returns `false` when the
/// bookmark doesn't exist or isn't in the trash.
///
/// A trashed bookmark is never restored on top of a live row: a fresh add
/// of the same URL can coexist with a trashed copy (the partial unique
/// index skips trash), but restoring would collide with it. That's a
/// friendly error naming the owner, not a raw UNIQUE-constraint failure —
/// the same message contract `insert`/`update` use, so the HTTP layer
/// classifies it as a 409.
pub fn restore(conn: &Connection, id: i64) -> Result<bool> {
	// Friendly collision pre-check: if a live (non-trashed) bookmark already
	// owns this URL, restoring the trashed copy would violate the partial
	// unique index. `find_url_owner` excludes `id` itself.
	let url: Option<String> = conn
		.query_row(
			"SELECT url FROM bookmarks WHERE id = ?1 AND trashed_at IS NOT NULL",
			params![id],
			|row| row.get(0),
		)
		.optional()?;
	let Some(url) = url else {
		crate::log_trace!("restore bookmark #{id}: false");
		return Ok(false);
	};
	if let Some((owner_id, owner_title)) = find_url_owner(conn, &url, id)? {
		anyhow::bail!(
			"URL already exists as bookmark #{owner_id} ({owner_title}) — you can't restore a \
			 copy on top of a live one; open bookmark #{owner_id} to view or edit it"
		);
	}

	// A concurrent writer can slip a live duplicate past the pre-check; the
	// constraint still fires, so fall back to the same friendly message.
	match conn.execute(
		"UPDATE bookmarks SET trashed_at = NULL WHERE id = ?1 AND trashed_at IS NOT NULL",
		params![id],
	) {
		Ok(changed) => {
			let ok = changed > 0;
			crate::log_trace!("restore bookmark #{id}: {ok}");
			Ok(ok)
		}
		Err(e) if is_unique_violation(&e) => Err(duplicate_error(conn, &url, None, id)),
		Err(e) => Err(e.into()),
	}
}

/// Permanently removes a bookmark, even one already in the trash (the
/// purge path of `DELETE /api/bookmarks/{id}`). The FTS delete trigger
/// cleans up the matching FTS row automatically (the
/// `WHEN OLD.trashed_at IS NULL` guard means purging trash never double-
/// `delete`s); ON DELETE CASCADE
/// cleans up bookmark_tags.
pub fn purge(conn: &Connection, id: i64) -> Result<bool> {
	let changed = conn.execute("DELETE FROM bookmarks WHERE id = ?1", params![id])?;
	let ok = changed > 0;
	crate::log_trace!("purge bookmark #{id}: {ok}");
	Ok(ok)
}

/// Criteria-based bulk remove: every bookmark matching `filter` is moved to
/// the trash, or permanently deleted when `purge` is true, all in one
/// transaction (all-or-nothing — a crash mid-way can't leave a half-trashed
/// set). `select_ids` picks the ids first, so the result always reports the
/// exact match set even when nothing was written.
///
/// A filter that matches nothing is a harmless no-op, not an error.
///
/// The transaction uses `Connection::unchecked_transaction` (the `&self`
/// variant) rather than `transaction(&mut self)` so callers that only hold
/// `&Connection` can use it. That's safe here because the connection is
/// never shared concurrently: the HTTP layer serializes it behind a
/// `Mutex`.
pub fn remove_matching(
	conn: &Connection,
	filter: &BookmarkFilter,
	purge: bool,
) -> Result<BulkRemoveResult> {
	let ids = select_ids(conn, filter)?;
	let mut removed = 0;
	if ids.is_empty() {
		crate::log_trace!("remove_matching: no rows match the filter");
		return Ok(BulkRemoveResult { ids, removed });
	}

	let tx = conn.unchecked_transaction()?;
	if purge {
		let mut stmt = tx.prepare("DELETE FROM bookmarks WHERE id = ?1")?;
		for id in &ids {
			removed += stmt.execute(params![id])? as i64;
		}
	} else {
		for id in &ids {
			// `trash_with_dedup` purges any older trashed copy of the URL, so
			// a criteria-based remove can't stack duplicates in the trash.
			if trash_with_dedup(&tx, *id)? {
				removed += 1;
			}
		}
	}
	tx.commit()?;
	crate::log_trace!(
		"remove_matching: {} matched, {} removed (purge={purge})",
		ids.len(),
		removed
	);
	Ok(BulkRemoveResult { ids, removed })
}

/// Removes a specific id list in one transaction, trashing or purging each
/// per `purge`. Stale ids are skipped rather than turning into errors, and
/// the returned `BulkRemoveResult` carries only the ids that actually
/// changed, so `dry_run`/result reporting stays truthful. Ids already in
/// the trash are ignored (a trashed bookmark can't be re-trashed).
pub fn remove_ids(conn: &Connection, ids: &[i64], purge: bool) -> Result<BulkRemoveResult> {
	if ids.is_empty() {
		return Ok(BulkRemoveResult {
			ids: Vec::new(),
			removed: 0,
		});
	}
	let tx = conn.unchecked_transaction()?;
	let mut removed = 0;
	let mut touched = Vec::new();
	if purge {
		let mut stmt = tx.prepare("DELETE FROM bookmarks WHERE id = ?1 AND trashed_at IS NULL")?;
		for id in ids {
			let affected = stmt.execute(params![id])?;
			removed += affected as i64;
			if affected > 0 {
				touched.push(*id);
			}
		}
	} else {
		for id in ids {
			if trash_with_dedup(&tx, *id)? {
				removed += 1;
				touched.push(*id);
			}
		}
	}
	tx.commit()?;
	crate::log_trace!(
		"remove_ids: removed {removed} of {} ids (purge={purge})",
		ids.len()
	);
	Ok(BulkRemoveResult {
		ids: touched,
		removed,
	})
}

// ============================================================
// Search (FTS)
// ============================================================

/// Applies the SQL-level search narrowing (category/tag/keyword) shared by
/// `search`, `search_archived`, and `count_search`. These three build their
/// WHERE clauses from the same source so the `x-total-count` header always
/// matches the results array — a narrowed search counts exactly the rows it
/// returns.
fn append_search_filters(
	filter: &BookmarkFilter,
	conditions: &mut Vec<String>,
	values: &mut Vec<Box<dyn rusqlite::ToSql>>,
) {
	if let Some(cat) = &filter.category {
		conditions.push("c.name = ?".into());
		values.push(Box::new(cat.clone()));
	}
	if let Some(tag) = &filter.tag {
		conditions.push(
			"b.id IN (SELECT bt.bookmark_id FROM bookmark_tags bt \
			  JOIN tags t ON t.id = bt.tag_id WHERE t.name = ?)"
				.into(),
		);
		values.push(Box::new(tag.clone()));
	}
	if let Some(keyword) = &filter.keyword {
		conditions.push("b.keyword = ? COLLATE NOCASE".into());
		values.push(Box::new(keyword.clone()));
	}
}

/// Full-text search over title/description/note/url.
///
/// The query is wrapped as an escaped FTS5 phrase (`"..."`, with internal
/// `"` doubled) before binding, rather than passed through as a raw FTS5
/// query string. Free-text user input containing characters that are
/// meaningful to FTS5 syntax — an unmatched quote, `*`, `:`, `NEAR`, a
/// column filter — would otherwise make MATCH return a runtime syntax
/// error instead of a search result.
///
/// Searches the main index only (`bookmarks_fts`), which holds active,
/// non-archived content. Results are ranked by FTS5 relevance. `filter`
/// narrows at the SQL level by category/tag/keyword (see
/// `append_search_filters`).
pub fn search(
	conn: &Connection,
	query: &str,
	limit: i64,
	filter: &BookmarkFilter,
) -> Result<Vec<Bookmark>> {
	let fts_query = format!("\"{}\"", query.replace('"', "\"\""));
	let mut conditions: Vec<String> = vec![
		"bookmarks_fts MATCH ?1".into(),
		"b.trashed_at IS NULL".into(),
		"b.is_archived = 0".into(),
	];
	let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query.clone())];
	append_search_filters(filter, &mut conditions, &mut values);
	let limit = limit.clamp(1, MAX_PAGE_SIZE);
	let sql = format!(
		"SELECT {SELECT_BOOKMARK_FIELDS}
         FROM bookmarks_fts f
         JOIN bookmarks b ON b.id = f.rowid
         LEFT JOIN categories c ON c.id = b.category_id
         WHERE {}
         ORDER BY rank
         LIMIT ?{}",
		conditions.join(" AND "),
		values.len() + 1
	);
	values.push(Box::new(limit));

	let mut stmt = conn.prepare(&sql)?;
	let param_refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|b| b.as_ref()).collect();
	let rows = stmt.query_map(param_refs.as_slice(), row_to_bookmark)?;
	let bookmarks = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!(
		"search {query:?} (fts query {fts_query:?}) -> {} results (limit {limit})",
		bookmarks.len()
	);
	attach_tags(conn, bookmarks)
}

/// Like `search`, but over the archive index: only archived (non-trashed)
/// bookmarks. Archived content lives in `bookmarks_fts_archived`,
/// physically separate from the main corpus, so this is the only place
/// archived bookmarks surface in search.
pub fn search_archived(
	conn: &Connection,
	query: &str,
	limit: i64,
	filter: &BookmarkFilter,
) -> Result<Vec<Bookmark>> {
	let fts_query = format!("\"{}\"", query.replace('"', "\"\""));
	let mut conditions: Vec<String> = vec![
		"bookmarks_fts_archived MATCH ?1".into(),
		"b.trashed_at IS NULL".into(),
		"b.is_archived = 1".into(),
	];
	let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query)];
	append_search_filters(filter, &mut conditions, &mut values);
	let limit = limit.clamp(1, MAX_PAGE_SIZE);
	let sql = format!(
		"SELECT {SELECT_BOOKMARK_FIELDS}
         FROM bookmarks_fts_archived f
         JOIN bookmarks b ON b.id = f.rowid
         LEFT JOIN categories c ON c.id = b.category_id
         WHERE {}
         ORDER BY rank
         LIMIT ?{}",
		conditions.join(" AND "),
		values.len() + 1
	);
	values.push(Box::new(limit));

	let mut stmt = conn.prepare(&sql)?;
	let param_refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|b| b.as_ref()).collect();
	let rows = stmt.query_map(param_refs.as_slice(), row_to_bookmark)?;
	let bookmarks = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!(
		"search (archive index) {query:?} -> {} results (limit {limit})",
		bookmarks.len()
	);
	attach_tags(conn, bookmarks)
}

/// Total number of bookmarks matching a full-text query in the given index.
/// `archived` picks the archive index (`bookmarks_fts_archived`) instead of
/// the main one, mirroring `search` vs `search_archived`. Used for the
/// `x-total-count` header on `/api/search`. `filter` applies the same
/// category/tag/keyword narrowing as the search itself.
pub fn count_search(
	conn: &Connection,
	query: &str,
	archived: bool,
	filter: &BookmarkFilter,
) -> Result<i64> {
	let table = if archived {
		"bookmarks_fts_archived"
	} else {
		"bookmarks_fts"
	};
	let fts_query = format!("\"{}\"", query.replace('"', "\"\""));
	let mut conditions: Vec<String> = vec![format!("{table} MATCH ?1")];
	let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query)];
	append_search_filters(filter, &mut conditions, &mut values);
	let sql = format!(
		"SELECT COUNT(*) FROM {table} f
		 JOIN bookmarks b ON b.id = f.rowid
		 LEFT JOIN categories c ON c.id = b.category_id
		 WHERE {}",
		conditions.join(" AND ")
	);
	let mut stmt = conn.prepare(&sql)?;
	let param_refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|b| b.as_ref()).collect();
	let total = stmt.query_row(param_refs.as_slice(), |row| row.get::<_, i64>(0))?;
	crate::log_trace!("count_search {query:?} (archive={archived}) -> {total}");
	Ok(total)
}
