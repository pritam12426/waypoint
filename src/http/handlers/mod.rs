/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! HTTP request handlers: one `pub async fn` per endpoint, plus the static
//! frontend fallback. Three patterns repeat throughout:
//!
//! * **Blocking DB work is off the async runtime** — each handler clones the
//!   `Arc<Mutex<Connection>>` and runs the query inside
//!   `tokio::task::spawn_blocking`. The `Mutex` guarantees only one task
//!   touches the connection at a time (SQLite connections aren't `Sync`).
//! * **Validation happens before the DB call** — id/limit/offset/keyword
//!   checks return `AppError` early so bad input never reaches SQL.
//! * **Client address is logged** — via the `ConnectInfo<SocketAddr>`
//!   extractor, enabled by `into_make_service_with_connect_info` in `run`.

use axum::{
	Json,
	body::Body,
	extract::{ConnectInfo, Path, Query, State},
	http::{HeaderMap, HeaderName, StatusCode, Uri, header},
	response::{IntoResponse, Redirect, Response},
};
use bytes::Bytes;
use percent_encoding::percent_decode_str;
use rusqlite::Connection;
use rust_embed::Embed;
use serde::Deserialize;
use std::borrow::Cow;
use std::net::SocketAddr;
use utoipa::IntoParams;

use super::{
	AppState, cursor,
	error::{ApiErrorBody, AppError},
};
use crate::database::{
	bookmarks as bm_db, categories as cat_db, stats as st_db, tags as tag_db, visits as vis_db,
};
use crate::model::{
	Bookmark, BookmarkFilter, BulkRemoveResult, BulkUpdateRequest, BulkUpdateResult, Category,
	DomainCount, DomainVisitStats, HygieneStats, MonthlyActivity, NeverVisitedBookmark,
	NewBookmark, OrphanTag, StatsOverview, TagCount, UpdateBookmark,
};
use crate::shared;

// ============================================================
// Shared validation
// ============================================================

/// Upper bound for `limit` on every list/search endpoint. Anything larger
/// is a client error (`invalid_limit`), not something to silently clamp.
const MAX_PAGE_SIZE: i64 = 1000;

/// `X-Total-Count` response header: total matches ignoring pagination.
/// Deliberately lowercase — axum 0.8 does not normalize header names, so a
/// capitalized constant would silently become a second, ignored header.
const X_TOTAL_COUNT: HeaderName = HeaderName::from_static("x-total-count");

/// `X-Next-Cursor` response header: opaque keyset token for the next page.
/// Same lowercase rule as `X_TOTAL_COUNT`.
const X_NEXT_CURSOR: HeaderName = HeaderName::from_static("x-next-cursor");

fn validate_limit(limit: Option<i64>) -> Result<i64, AppError> {
	match limit {
		None => Ok(200),
		Some(l) if (1..=MAX_PAGE_SIZE).contains(&l) => Ok(l),
		Some(l) => Err(AppError::invalid_limit(format!(
			"limit must be between 1 and {MAX_PAGE_SIZE}, got {l}"
		))),
	}
}

fn validate_offset(offset: Option<i64>) -> Result<i64, AppError> {
	match offset {
		None => Ok(0),
		Some(o) if o >= 0 => Ok(o),
		Some(o) => Err(AppError::invalid_offset(format!(
			"offset must be 0 or greater, got {o}"
		))),
	}
}

/// `limit` for the stats sub-resources. Unlike the list endpoints there is
/// no single natural default (domains: 50, top-visited: 20, activity: 12),
/// so callers pass theirs in; the range and error contract stay identical
/// to `validate_limit`.
fn validate_stats_limit(limit: Option<i64>, default: i64) -> Result<i64, AppError> {
	match limit {
		None => Ok(default),
		Some(l) if (1..=MAX_PAGE_SIZE).contains(&l) => Ok(l),
		Some(l) => Err(AppError::invalid_limit(format!(
			"limit must be between 1 and {MAX_PAGE_SIZE}, got {l}"
		))),
	}
}

fn validate_id(id: i64) -> Result<i64, AppError> {
	if id < 1 {
		Err(AppError::invalid_id(format!(
			"id must be a positive integer, got {id}"
		)))
	} else {
		Ok(id)
	}
}

/// Keywords become URL path segments at `/keywords/{keyword}`, so they are
/// restricted to the same safe charset a path segment tolerates.
fn validate_keyword(keyword: Option<&str>) -> Result<(), AppError> {
	if let Some(k) = keyword
		&& !k.is_empty()
		&& !k
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
	{
		return Err(AppError::invalid_keyword(
			"keyword may only contain letters, digits, '.', '_' and '-'",
		));
	}
	Ok(())
}

/// Parses an optional `--*-after`/`--*-before` query string into the
/// normalized `YYYY-MM-DD HH:MM:SS` UTC form `BookmarkFilter` expects.
/// `end_of_day` picks day-start for `*_after` bounds and day-end for
/// `*_before` bounds so a bare date is inclusive of the whole day. A bad
/// value is a 400 (`invalid_date`) instead of a SQL error.
fn parse_bound(value: Option<String>, end_of_day: bool) -> Result<Option<String>, AppError> {
	shared::parse_datetime_bound_option(value, end_of_day).map_err(AppError::invalid_date)
}

/// Enforces that each normalized `*_after` / `*_before` pair is a sane range
/// (after must not sort after before). The values are already fixed-width
/// `YYYY-MM-DD HH:MM:SS` UTC strings from `parse_bound`, so plain
/// lexicographic comparison is chronological. An inverted range is a 400
/// (`invalid_date`) rather than a silently-empty list.
fn validate_bounds(
	created: (Option<String>, Option<String>),
	updated: (Option<String>, Option<String>),
	visited: (Option<String>, Option<String>),
	trashed: (Option<String>, Option<String>),
) -> Result<(), AppError> {
	for (label, (after, before)) in [
		("created", created),
		("updated", updated),
		("last_visited", visited),
		("trashed", trashed),
	] {
		shared::validate_time_range(after.as_deref(), before.as_deref(), label)
			.map_err(AppError::invalid_date)?;
	}
	Ok(())
}

// ============================================================
// Cached aggregate responses
// ============================================================

/// Cache key for a paged aggregate endpoint. The underlying queries are
/// full-corpus GROUP BY / ORDER BY passes, so they are cached server-side
/// (see `StatsCache`); the key must include pagination because a 50-row
/// slice and a 200-row slice are different queries.
fn stats_key(endpoint: &str, limit: i64, offset: i64) -> String {
	format!("{endpoint}:{limit}:{offset}")
}

/// Runs `compute` against the database and serves its JSON through the
/// stats cache (30s TTL). Emits `Cache-Control: private, max-age=30`
/// (matching the server TTL, so the browser can serve the dashboard from
/// its own cache) and a strong ETag; a matching `If-None-Match` short-
/// circuits to 304.
async fn cached_json<T>(
	state: &AppState,
	key: String,
	if_none_match: Option<&str>,
	compute: impl FnOnce(&Connection) -> anyhow::Result<T> + Send + 'static,
) -> Result<Response, AppError>
where
	T: serde::Serialize,
{
	if let Some(body) = state.stats.get(&key) {
		crate::log_trace!("cached_json: served {key:?} from stats cache");
		return Ok(etag_response(body, if_none_match));
	}
	crate::log_trace!("cached_json: computing {key:?} (stats cache miss)");
	let db = state.db.clone();
	let body = tokio::task::spawn_blocking(move || {
		let conn = db.reader();
		let value = compute(&conn)?;
		Ok::<_, anyhow::Error>(serde_json::to_vec(&value)?)
	})
	.await??;
	state.stats.put(&key, body.clone());
	Ok(etag_response(body, if_none_match))
}

/// Wraps JSON bytes with cache headers and honors `If-None-Match`.
fn etag_response(body: Vec<u8>, if_none_match: Option<&str>) -> Response {
	let etag = body_etag(&body);
	if if_none_match == Some(etag.as_str()) {
		crate::log_trace!("etag matched: responding 304 Not Modified");
		return StatusCode::NOT_MODIFIED.into_response();
	}
	crate::log_trace!("etag {}: sending body", etag);
	(
		[
			(header::CONTENT_TYPE, "application/json"),
			(header::CACHE_CONTROL, "private, max-age=30"),
			(header::ETAG, etag.as_str()),
		],
		body,
	)
		.into_response()
}

/// Deterministic strong ETag from the body bytes. `DefaultHasher` uses the
/// fixed-key SipHash, so the value is stable across processes — only the
/// body changes it, which is all a revalidation ETag needs.
fn body_etag(body: &[u8]) -> String {
	use std::collections::hash_map::DefaultHasher;
	use std::hash::{Hash, Hasher};
	let mut hasher = DefaultHasher::new();
	body.hash(&mut hasher);
	format!("\"{:016x}\"", hasher.finish())
}

// ============================================================
// Bookmarks
// ============================================================

#[derive(Deserialize, IntoParams)]
pub struct ListQuery {
	/// Filter by category name.
	category: Option<String>,
	/// Filter by category id.
	category_id: Option<i64>,
	/// Filter by tag name.
	tag: Option<String>,
	/// Filter by keyword shortcut.
	keyword: Option<String>,
	/// Filter by starred state.
	starred: Option<bool>,
	/// `true` lists only archived bookmarks, `false` only active ones.
	archived: Option<bool>,
	/// List trashed bookmarks (overrides `archived`).
	trash: Option<bool>,
	/// Only bookmarks created at or after this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]]).
	created_after: Option<String>,
	/// Only bookmarks created at or before this UTC date/time.
	created_before: Option<String>,
	/// Only bookmarks updated at or after this UTC date/time.
	updated_after: Option<String>,
	/// Only bookmarks updated at or before this UTC date/time.
	updated_before: Option<String>,
	/// Only bookmarks last visited at or after this UTC date/time.
	visited_after: Option<String>,
	/// Only bookmarks last visited at or before this UTC date/time.
	visited_before: Option<String>,
	/// Only bookmarks trashed at or after this UTC date/time (with `trash=true`).
	trashed_after: Option<String>,
	/// Only bookmarks trashed at or before this UTC date/time (with `trash=true`).
	trashed_before: Option<String>,
	/// Maximum number of results (1–1000, default 200).
	limit: Option<i64>,
	/// Number of results to skip. Cursor pagination supersedes this: when
	/// `cursor` is present, `offset` is ignored.
	offset: Option<i64>,
	/// Opaque keyset token returned in the `x-next-cursor` header of a
	/// previous page. Constant-time deep pagination (index range SEARCH) as
	/// opposed to the OFFSET walk. Only valid for the active (non-trash)
	/// list.
	cursor: Option<String>,
}

/// Pagination shared by the paged stats sub-resources. `limit` defaults
/// differ per endpoint (see each handler); `offset` always defaults to 0.
#[derive(Deserialize, IntoParams)]
pub struct StatsQuery {
	/// Maximum number of results.
	limit: Option<i64>,
	/// Number of results to skip.
	offset: Option<i64>,
}

/// List bookmarks, optionally filtered.
#[utoipa::path(
	get,
	path = "/bookmarks",
	tag = "bookmarks",
	params(ListQuery),
	responses(
		(
			status = 200,
			description = "Matching bookmarks. Trashed bookmarks are excluded unless `trash=true`.",
			body = [Bookmark],
			headers(
				("x-total-count" = i64, description = "Total matching bookmarks, ignoring limit/offset"),
				("x-next-cursor" = String, description = "Opaque token for the next page. Present when the page is full (i.e. there may be more rows); pass it as the `cursor` query parameter."),
			),
		),
		(status = 400, description = "Invalid query parameters", body = ApiErrorBody),
	)
)]
pub async fn list_bookmarks(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Query(q): Query<ListQuery>,
) -> Result<Response, AppError> {
	let limit = validate_limit(q.limit)?;
	let offset = validate_offset(q.offset)?;
	// The cursor is an opaque token (see `http::cursor`); decode it into the
	// keyset bound. A malformed token is a 400, mirroring the other query
	// validation. Trash never paginates by cursor.
	let before_cursor = match q.cursor {
		Some(token) => {
			if q.trash.unwrap_or(false) {
				return Err(AppError::invalid_limit(
					"cursor pagination is only valid for the active list",
				));
			}
			match cursor::decode_cursor(&token) {
				Some(bound) => Some(bound),
				None => {
					return Err(AppError::invalid_limit("invalid cursor token"));
				}
			}
		}
		None => None,
	};
	// Time bounds are normalized to fixed-width `YYYY-MM-DD HH:MM:SS` UTC
	// by `shared::parse_datetime_bound`; a bare date means the whole day
	// (day-start for `*_after`, day-end for `*_before`). Garbage input is a
	// 400 with code `invalid_date`.
	let created_after = parse_bound(q.created_after, false)?;
	let created_before = parse_bound(q.created_before, true)?;
	let updated_after = parse_bound(q.updated_after, false)?;
	let updated_before = parse_bound(q.updated_before, true)?;
	let visited_after = parse_bound(q.visited_after, false)?;
	let visited_before = parse_bound(q.visited_before, true)?;
	let trashed_after = parse_bound(q.trashed_after, false)?;
	let trashed_before = parse_bound(q.trashed_before, true)?;
	// An inverted range is a 400 (`invalid_date`) rather than a
	// silently-empty list.
	validate_bounds(
		(created_after.clone(), created_before.clone()),
		(updated_after.clone(), updated_before.clone()),
		(visited_after.clone(), visited_before.clone()),
		(trashed_after.clone(), trashed_before.clone()),
	)?;
	// A trashed filter must not also filter on live-ness: `trash` overrides
	// `archived` (the docs say so), and time bounds are only meaningful
	// alongside their column.
	let filter = BookmarkFilter {
		category: q.category,
		category_id: q.category_id,
		tag: q.tag,
		keyword: q.keyword,
		starred: q.starred,
		archived: q.archived,
		trash: q.trash.unwrap_or(false),
		created_after,
		created_before,
		updated_after,
		updated_before,
		last_visited_after: visited_after,
		last_visited_before: visited_before,
		trashed_after,
		trashed_before,
		limit: Some(limit),
		// Cursor and offset are mutually exclusive; the cursor wins.
		offset: before_cursor.as_ref().map_or(Some(offset), |_| None),
		before_cursor,
	};
	let db = state.db.clone();
	let counts = state.counts.clone();
	let trash = filter.trash;
	// The count cache key ignores pagination: `count` never uses limit/
	// offset or the cursor bound, so every page of a filter must hit the
	// same entry as page 1.
	let count_key = {
		let mut key_filter = filter.clone();
		key_filter.limit = None;
		key_filter.offset = None;
		key_filter.before_cursor = None;
		format!("{key_filter:?}")
	};
	// `list` and `count` run together inside one spawn_blocking so they see
	// a consistent snapshot and share the lock acquisition. `count` mirrors
	// `list`'s WHERE clause (see `database::bookmarks`) so the header is an
	// exact total, not an approximation.
	let (bookmarks, total) = tokio::task::spawn_blocking(move || {
		let conn = db.reader();
		let list = bm_db::list(&conn, &filter)?;
		let total = match counts.get(&count_key) {
			Some(total) => total,
			None => {
				let total = bm_db::count(&conn, &filter)?;
				counts.put(&count_key, total);
				total
			}
		};
		Ok::<_, anyhow::Error>((list, total))
	})
	.await??;
	crate::log_debug!(
		"{addr} GET /api/bookmarks{}: returned {} of {} bookmarks",
		if trash { "?trash=true" } else { "" },
		bookmarks.len(),
		total
	);
	// A full page gets an `x-next-cursor` token pointing at its last row;
	// a short page is the last one, so the header is omitted. Only the
	// active list paginates by cursor.
	let next_cursor = if !trash && !bookmarks.is_empty() && bookmarks.len() == limit as usize {
		let last = bookmarks.last().unwrap();
		Some(cursor::encode_cursor(last.id, &last.created_at))
	} else {
		None
	};
	Ok(match next_cursor {
		Some(next) => (
			[(X_TOTAL_COUNT, total.to_string()), (X_NEXT_CURSOR, next)],
			Json(bookmarks),
		)
			.into_response(),
		None => ([(X_TOTAL_COUNT, total.to_string())], Json(bookmarks)).into_response(),
	})
}

/// Create a new bookmark.
#[utoipa::path(
	post,
	path = "/bookmarks",
	tag = "bookmarks",
	request_body = NewBookmark,
	responses(
		(status = 201, description = "Bookmark created", body = Bookmark),
		(status = 400, description = "Invalid payload", body = ApiErrorBody),
		(status = 409, description = "A bookmark with this URL or keyword already exists", body = ApiErrorBody),
	)
)]
pub async fn create_bookmark(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Json(new): Json<NewBookmark>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} POST /api/bookmarks: {}", new.url);
	let created_url = new.url.clone();
	// URL is the only truly required field on `NewBookmark`; validate it
	// and the (optional) keyword before touching the database.
	if new.url.trim().is_empty() {
		return Err(AppError::invalid_url("url is required"));
	}
	validate_keyword(new.keyword.as_deref())?;

	// Friendly duplicate rejection first, on a *reader*: a colliding save
	// must not trigger a needless network fetch or cache write. Media
	// resolution (potentially network I/O) then runs on its own blocking
	// task, still without the writer; only the final tight INSERT takes it.
	let db = state.db.clone();
	let new_for_check = new.clone();
	tokio::task::spawn_blocking(move || {
		bm_db::check_insert_collisions(&db.reader(), &new_for_check)
	})
	.await??;

	let new_for_resolve = new.clone();
	let media =
		tokio::task::spawn_blocking(move || crate::core::media::resolve_new(&new_for_resolve))
			.await?
			.map_err(AppError::invalid_payload)?;

	let db = state.db.clone();
	let new_for_db = new.clone();
	let id = tokio::task::spawn_blocking(move || {
		bm_db::insert_resolved(&db.writer(), &new_for_db, media)
	})
	.await??;
	state.invalidate_caches();
	crate::log_info!("{addr} created bookmark #{id}: {created_url}");

	// Re-fetch to return the fully hydrated bookmark (with tags attached
	// and the category name resolved) rather than echoing the input.
	let db = state.db.clone();
	let bookmark = tokio::task::spawn_blocking(move || bm_db::get(&db.reader(), id)).await??;

	Ok((StatusCode::CREATED, Json(bookmark)).into_response())
}

/// Fetch a single bookmark.
#[utoipa::path(
	get,
	path = "/bookmarks/{id}",
	tag = "bookmarks",
	params(("id" = i64, Path, description = "Bookmark id")),
	responses(
		(status = 200, description = "Bookmark found", body = Bookmark),
		(status = 404, description = "Bookmark not found", body = ApiErrorBody),
	)
)]
pub async fn get_bookmark(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<i64>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /api/bookmarks/{id}");
	validate_id(id)?;
	let db = state.db.clone();
	let bookmark = tokio::task::spawn_blocking(move || bm_db::get(&db.reader(), id)).await??;
	match bookmark {
		Some(b) => Ok(Json(b).into_response()),
		None => Err(AppError::not_found("bookmark not found")),
	}
}

/// Update a bookmark (partial update: omitted fields are unchanged).
#[utoipa::path(
	put,
	path = "/bookmarks/{id}",
	tag = "bookmarks",
	params(("id" = i64, Path, description = "Bookmark id")),
	request_body = UpdateBookmark,
	responses(
		(status = 200, description = "Updated bookmark", body = Bookmark),
		(status = 400, description = "Invalid payload", body = ApiErrorBody),
		(status = 404, description = "Bookmark not found", body = ApiErrorBody),
		(status = 409, description = "A bookmark with this URL or keyword already exists", body = ApiErrorBody),
	)
)]
pub async fn update_bookmark(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<i64>,
	Json(update): Json<UpdateBookmark>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} PUT /api/bookmarks/{id}");
	validate_id(id)?;
	// Partial-update contract: an empty-string URL means "clear it", which
	// is invalid; a `None` URL means "unchanged". Same gate as the CLI.
	if update.url.as_deref().is_some_and(|u| u.trim().is_empty()) {
		return Err(AppError::invalid_url("url cannot be empty"));
	}
	validate_keyword(update.keyword.as_deref())?;

	// Read the current row first (a reader; no writer yet) — media
	// resolution needs it, and a missing id must 404 before any fetch.
	let db = state.db.clone();
	let existing = tokio::task::spawn_blocking(move || bm_db::get(&db.reader(), id)).await??;
	let Some(existing) = existing else {
		return Err(AppError::not_found("bookmark not found"));
	};

	// Duplicate checks and media resolution (potentially network I/O) run
	// without the writer held; only `update_resolved` takes it, and it
	// re-checks against the fresh row so a concurrent edit can't slip a
	// duplicate in or persist stale media.
	let db = state.db.clone();
	let seen = existing.clone();
	let update_for_check = update.clone();
	tokio::task::spawn_blocking(move || {
		bm_db::check_update_collisions(&db.reader(), &seen, &update_for_check)
	})
	.await??;

	let seen = existing.clone();
	let update_for_resolve = update.clone();
	let media = tokio::task::spawn_blocking(move || {
		crate::core::media::resolve_update(&seen, &update_for_resolve)
	})
	.await?
	.map_err(AppError::invalid_payload)?;

	let db = state.db.clone();
	let seen = existing.clone();
	let update_for_db = update.clone();
	let existing = tokio::task::spawn_blocking(move || {
		bm_db::update_resolved(&db.writer(), id, &update_for_db, &seen, media)
	})
	.await??;

	let Some(existing) = existing else {
		return Err(AppError::not_found("bookmark not found"));
	};
	state.invalidate_caches();
	// `describe` diffs the pre-update bookmark against the request so the
	// change log shows exactly what moved ("title: A -> B", "starred: no ->
	// yes"), matching the CLI's `update` output.
	let changes = update.describe(&existing);
	let changes = if changes.is_empty() {
		"no changes".to_string()
	} else {
		changes.join(", ")
	};
	crate::log_info!("{addr} updated bookmark #{id} ({changes})");

	let db = state.db.clone();
	let bookmark = tokio::task::spawn_blocking(move || bm_db::get(&db.reader(), id)).await??;
	Ok(Json(bookmark).into_response())
}

#[derive(Deserialize, IntoParams)]
pub struct DeleteQuery {
	/// Permanently delete instead of moving to trash.
	purge: Option<bool>,
}

/// Remove a bookmark. Moves it to the trash by default; `purge=true`
/// deletes it permanently.
#[utoipa::path(
	delete,
	path = "/bookmarks/{id}",
	tag = "bookmarks",
	params(("id" = i64, Path, description = "Bookmark id"), DeleteQuery),
	responses(
		(status = 204, description = "Bookmark removed"),
		(status = 404, description = "Bookmark not found", body = ApiErrorBody),
	)
)]
pub async fn delete_bookmark(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<i64>,
	Query(q): Query<DeleteQuery>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} DELETE /api/bookmarks/{id}");
	validate_id(id)?;
	let db = state.db.clone();
	let purge = q.purge.unwrap_or(false);
	let removed = if purge {
		tokio::task::spawn_blocking(move || bm_db::purge(&db.writer(), id)).await??
	} else {
		tokio::task::spawn_blocking(move || bm_db::trash(&db.writer(), id)).await??
	};

	if removed {
		state.invalidate_caches();
		if purge {
			crate::log_info!("{addr} purged bookmark #{id}");
		} else {
			crate::log_info!("{addr} moved bookmark #{id} to trash");
		}
	}
	Ok(if removed {
		StatusCode::NO_CONTENT.into_response()
	} else {
		return Err(AppError::not_found("bookmark not found"));
	})
}

/// Restore a bookmark from the trash.
#[utoipa::path(
	post,
	path = "/bookmarks/{id}/restore",
	tag = "bookmarks",
	params(("id" = i64, Path, description = "Bookmark id")),
	responses(
		(status = 204, description = "Bookmark restored from trash"),
		(status = 404, description = "Bookmark not found", body = ApiErrorBody),
	)
)]
pub async fn restore_bookmark(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<i64>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} POST /api/bookmarks/{id}/restore");
	validate_id(id)?;
	let db = state.db.clone();
	let restored = tokio::task::spawn_blocking(move || bm_db::restore(&db.writer(), id)).await??;
	if restored {
		state.invalidate_caches();
	}
	Ok(if restored {
		crate::log_info!("{addr} restored bookmark #{id}");
		StatusCode::NO_CONTENT.into_response()
	} else {
		return Err(AppError::not_found("bookmark not found"));
	})
}

/// Query parameters for the bulk `DELETE /api/bookmarks` endpoint: either a
/// comma-separated `ids` list or filter criteria (mutually exclusive), plus
/// `purge` and `dry_run`. The criteria mirror `ListQuery`'s so a GET
/// preview and a DELETE use the same shape.
#[derive(Deserialize, IntoParams)]
pub struct BulkDeleteQuery {
	/// Comma-separated bookmark ids to remove (mutually exclusive with the
	/// filter criteria below).
	ids: Option<String>,
	/// Filter by category name.
	category: Option<String>,
	/// Filter by category id.
	category_id: Option<i64>,
	/// Filter by tag name.
	tag: Option<String>,
	/// Filter by keyword shortcut.
	keyword: Option<String>,
	/// Filter by starred state.
	starred: Option<bool>,
	/// `true` targets only archived bookmarks, `false` only active ones.
	archived: Option<bool>,
	/// Target trashed bookmarks instead of live ones (for bulk purge of
	/// the recycle bin).
	trash: Option<bool>,
	/// Only bookmarks created at or after this UTC date/time.
	created_after: Option<String>,
	/// Only bookmarks created at or before this UTC date/time.
	created_before: Option<String>,
	/// Only bookmarks updated at or after this UTC date/time.
	updated_after: Option<String>,
	/// Only bookmarks updated at or before this UTC date/time.
	updated_before: Option<String>,
	/// Only bookmarks last visited at or after this UTC date/time.
	visited_after: Option<String>,
	/// Only bookmarks last visited at or before this UTC date/time.
	visited_before: Option<String>,
	/// Only bookmarks trashed at or after this UTC date/time.
	trashed_after: Option<String>,
	/// Only bookmarks trashed at or before this UTC date/time.
	trashed_before: Option<String>,
	/// Permanently delete instead of moving to the trash.
	purge: Option<bool>,
	/// Report the matching ids/count without changing anything.
	dry_run: Option<bool>,
}

/// Bulk-remove bookmarks by id list or filter criteria. Never a catch-all:
/// calling this without `ids` *and* without any criterion is a 400, so a
/// bare `DELETE /api/bookmarks` cannot silently gut the database. With
/// `dry_run=true` the matching ids and count come back with `removed: 0`.
#[utoipa::path(
	delete,
	path = "/bookmarks",
	tag = "bookmarks",
	params(BulkDeleteQuery),
	responses(
		(
			status = 200,
			description = "Bookmarks removed (or, with dry_run=true, the ids that would be removed)",
			body = BulkRemoveResult,
		),
		(status = 400, description = "No ids and no criteria, or invalid parameters", body = ApiErrorBody),
	)
)]
pub async fn bulk_delete_bookmarks(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Query(q): Query<BulkDeleteQuery>,
) -> Result<Json<BulkRemoveResult>, AppError> {
	let dry_run = q.dry_run.unwrap_or(false);
	let purge = q.purge.unwrap_or(false);

	let ids: Vec<i64> = match q.ids {
		Some(raw) => raw
			.split(',')
			.filter(|part| !part.is_empty())
			.map(|part| {
				let id = part
					.parse::<i64>()
					.map_err(|_| AppError::invalid_id(format!("invalid bookmark id: {part}")))?;
				validate_id(id)
			})
			.collect::<Result<Vec<_>, AppError>>()?,
		None => Vec::new(),
	};

	let has_criteria = [
		q.category.is_some(),
		q.category_id.is_some(),
		q.tag.is_some(),
		q.keyword.is_some(),
		q.starred.is_some(),
		q.archived.is_some(),
		q.trash.is_some(),
		q.created_after.is_some(),
		q.created_before.is_some(),
		q.updated_after.is_some(),
		q.updated_before.is_some(),
		q.visited_after.is_some(),
		q.visited_before.is_some(),
		q.trashed_after.is_some(),
		q.trashed_before.is_some(),
	]
	.iter()
	.any(|c| *c);

	if ids.is_empty() && !has_criteria {
		return Err(AppError::invalid_limit(
			"bulk delete needs ids or at least one filter criterion \
			 (refusing a catch-all)",
		));
	}
	if !ids.is_empty() && has_criteria {
		return Err(AppError::invalid_limit(
			"bulk delete accepts either an ids list or filter criteria, not both",
		));
	}

	let filter = if has_criteria {
		let created_after = parse_bound(q.created_after, false)?;
		let created_before = parse_bound(q.created_before, true)?;
		let updated_after = parse_bound(q.updated_after, false)?;
		let updated_before = parse_bound(q.updated_before, true)?;
		let visited_after = parse_bound(q.visited_after, false)?;
		let visited_before = parse_bound(q.visited_before, true)?;
		let trashed_after = parse_bound(q.trashed_after, false)?;
		let trashed_before = parse_bound(q.trashed_before, true)?;
		validate_bounds(
			(created_after.clone(), created_before.clone()),
			(updated_after.clone(), updated_before.clone()),
			(visited_after.clone(), visited_before.clone()),
			(trashed_after.clone(), trashed_before.clone()),
		)?;
		BookmarkFilter {
			category: q.category,
			category_id: q.category_id,
			tag: q.tag,
			keyword: q.keyword,
			starred: q.starred,
			archived: q.archived,
			trash: q.trash.unwrap_or(false),
			created_after,
			created_before,
			updated_after,
			updated_before,
			last_visited_after: visited_after,
			last_visited_before: visited_before,
			trashed_after,
			trashed_before,
			limit: None,
			offset: None,
			before_cursor: None,
		}
	} else {
		BookmarkFilter::default()
	};

	crate::log_debug!(
		"{addr} DELETE /api/bookmarks (ids={ids:?}, criteria={has_criteria}, purge={purge}, dry_run={dry_run})"
	);
	let db = state.db.clone();
	let result = match tokio::task::spawn_blocking(move || -> anyhow::Result<BulkRemoveResult> {
		let conn = db.writer();
		if dry_run {
			if has_criteria {
				// Preview mode reports the ids a real run would touch.
				let matched = bm_db::select_ids(&conn, &filter)?;
				Ok(BulkRemoveResult {
					ids: matched,
					removed: 0,
				})
			} else {
				Ok(BulkRemoveResult {
					ids: ids.clone(),
					removed: 0,
				})
			}
		} else if has_criteria {
			Ok(bm_db::remove_matching(&conn, &filter, purge)?)
		} else {
			Ok(bm_db::remove_ids(&conn, &ids, purge)?)
		}
	})
	.await
	{
		Ok(Ok(result)) => result,
		Ok(Err(err)) => {
			crate::log_error!("{addr} DELETE /api/bookmarks failed: {err:#}");
			return Err(AppError::internal());
		}
		Err(join) => {
			crate::log_error!("{addr} DELETE /api/bookmarks task panicked: {join}");
			return Err(AppError::internal());
		}
	};

	crate::log_info!(
		"{addr} bulk delete: removed {} bookmark(s) (dry_run={dry_run})",
		result.removed
	);
	if !dry_run && result.removed > 0 {
		state.invalidate_caches();
	}
	Ok(Json(result))
}

/// Apply one partial update to many bookmarks by id. All ids receive the
/// same change; ids that don't exist or are trashed are reported in
/// `skipped` instead of failing the request.
///
/// Media-affecting fields (`url`, `favicon`, `thumbnail`, modes, `refresh`)
/// resolve per bookmark without holding the writer, mirroring
/// `update_bookmark`. Validation failures caught in the pre-write pass
/// (empty payload, duplicate URL/keyword, unresolvable media) abort before
/// anything is written, so a bad id can't leave a half-applied batch; a
/// collision that only materializes at write time (a concurrent edit)
/// aborts mid-batch.
#[utoipa::path(
	patch,
	path = "/bookmarks",
	tag = "bookmarks",
	request_body = BulkUpdateRequest,
	responses(
		(
			status = 200,
			description = "Bulk update applied",
			body = BulkUpdateResult,
		),
		(
			status = 400,
			description = "No ids, nothing to change, or invalid parameters",
			body = ApiErrorBody,
		),
		(
			status = 409,
			description = "A bookmark with this URL or keyword already exists",
			body = ApiErrorBody,
		),
	)
)]
pub async fn bulk_update_bookmarks(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Json(req): Json<BulkUpdateRequest>,
) -> Result<Json<BulkUpdateResult>, AppError> {
	crate::log_debug!("{addr} PATCH /api/bookmarks ({} ids)", req.ids.len());
	if req.ids.is_empty() {
		return Err(AppError::invalid_payload(
			"bulk update needs at least one bookmark id",
		));
	}
	for id in &req.ids {
		validate_id(*id)?;
	}
	let update = req.update.clone();
	// Same gates as the single-update handler, before any id is touched.
	if !update.has_any_change() {
		return Err(AppError::invalid_payload(
			"bulk update has nothing to change",
		));
	}
	if update.url.as_deref().is_some_and(|u| u.trim().is_empty()) {
		return Err(AppError::invalid_url("url cannot be empty"));
	}
	validate_keyword(update.keyword.as_deref())?;

	// Pass 1 (off the writer): read each row, duplicate-check, resolve
	// media. Any failure here aborts before a single write, so a bad id or
	// a duplicate in the batch can't leave earlier ids half-applied.
	// Trashed or missing ids are collected as `skipped`, never a hard error.
	let mut pending: Vec<(i64, Bookmark, crate::core::media::ResolvedMedia)> = Vec::new();
	let mut skipped: Vec<i64> = Vec::new();
	for id in req.ids {
		let db = state.db.clone();
		let existing = tokio::task::spawn_blocking(move || bm_db::get(&db.reader(), id)).await??;
		let Some(existing) = existing else {
			skipped.push(id);
			continue;
		};
		let db = state.db.clone();
		let seen = existing.clone();
		let update_for_check = update.clone();
		tokio::task::spawn_blocking(move || {
			bm_db::check_update_collisions(&db.reader(), &seen, &update_for_check)
		})
		.await??;
		let seen = existing.clone();
		let update_for_resolve = update.clone();
		let media = tokio::task::spawn_blocking(move || {
			crate::core::media::resolve_update(&seen, &update_for_resolve)
		})
		.await?
		.map_err(AppError::invalid_payload)?;
		pending.push((id, existing, media));
	}

	// Pass 2 (writer): persist each pre-resolved update. A bookmark that
	// vanished between the passes (concurrently trashed) is skipped.
	let mut updated: Vec<i64> = Vec::new();
	for (id, seen, media) in pending {
		let db = state.db.clone();
		let update_for_db = update.clone();
		let result = tokio::task::spawn_blocking(move || {
			bm_db::update_resolved(&db.writer(), id, &update_for_db, &seen, media)
		})
		.await??;
		match result {
			Some(_) => updated.push(id),
			None => skipped.push(id),
		}
	}

	crate::log_info!(
		"{addr} bulk update: updated {} bookmark(s), skipped {}",
		updated.len(),
		skipped.len()
	);
	if !updated.is_empty() {
		state.invalidate_caches();
	}
	Ok(Json(BulkUpdateResult { updated, skipped }))
}

/// Permanently empty the trash. With `before`, only bookmarks trashed at or
/// before that UTC date/time are purged. No confirmation is needed — the
/// frontend gates the destructive call behind its own dialog. With
/// `dry_run=true` the matching ids come back with `removed: 0`.
#[utoipa::path(
	delete,
	path = "/trash",
	tag = "trash",
	params(("before" = Option<String>, Query, description = "Only purge bookmarks trashed at or before this UTC date/time")),
	responses(
		(
			status = 200,
			description = "Trashed bookmarks permanently deleted (or, with dry_run=true, the ids that would be purged)",
			body = BulkRemoveResult,
		),
		(status = 400, description = "Invalid date", body = ApiErrorBody),
	)
)]
pub async fn empty_trash(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Query(q): Query<EmptyTrashQuery>,
) -> Result<Json<BulkRemoveResult>, AppError> {
	let before = parse_bound(q.before, true)?;
	let dry_run = q.dry_run.unwrap_or(false);
	crate::log_debug!("{addr} DELETE /api/trash (before={before:?}, dry_run={dry_run})");
	let filter = BookmarkFilter {
		trash: true,
		trashed_before: before,
		limit: None,
		offset: None,
		..Default::default()
	};
	let db = state.db.clone();
	let result = match tokio::task::spawn_blocking(move || {
		let conn = db.writer();
		if dry_run {
			let matched = bm_db::select_ids(&conn, &filter)?;
			Ok::<BulkRemoveResult, anyhow::Error>(BulkRemoveResult {
				ids: matched,
				removed: 0,
			})
		} else {
			bm_db::remove_matching(&conn, &filter, true)
		}
	})
	.await
	{
		Ok(Ok(result)) => result,
		Ok(Err(err)) => {
			crate::log_error!("{addr} DELETE /api/trash failed: {err:#}");
			return Err(AppError::internal());
		}
		Err(join) => {
			crate::log_error!("{addr} DELETE /api/trash task panicked: {join}");
			return Err(AppError::internal());
		}
	};
	crate::log_info!(
		"{addr} trash emptied: permanently deleted {} bookmark(s) (dry_run={dry_run})",
		result.removed
	);
	if !dry_run && result.removed > 0 {
		state.invalidate_caches();
	}
	Ok(Json(result))
}

#[derive(Deserialize, IntoParams)]
pub struct EmptyTrashQuery {
	/// Only purge bookmarks trashed at or before this UTC date/time.
	before: Option<String>,
	/// Report the matching ids/count without changing anything.
	dry_run: Option<bool>,
}

// ============================================================
// Categories / tags / search / stats
// ============================================================

/// List all categories.
#[utoipa::path(
	get,
	path = "/categories",
	tag = "categories",
	responses((status = 200, description = "All categories", body = [Category])),
)]
pub async fn list_categories(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<Vec<Category>>, AppError> {
	crate::log_debug!("{addr} GET /api/categories");
	let db = state.db.clone();
	let categories = tokio::task::spawn_blocking(move || cat_db::list(&db.reader())).await??;
	crate::log_debug!("{addr} listed {} categories", categories.len());
	Ok(Json(categories))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RenameCategory {
	name: String,
}

/// Rename a category. All bookmarks in this category move with it.
#[utoipa::path(
	put,
	path = "/categories/{id}",
	tag = "categories",
	params(("id" = i64, Path, description = "Category id")),
	request_body = RenameCategory,
	responses(
		(status = 200, description = "Category renamed"),
		(status = 400, description = "Invalid name", body = ApiErrorBody),
		(status = 404, description = "Category not found", body = ApiErrorBody),
	)
)]
pub async fn rename_category(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<i64>,
	Json(body): Json<RenameCategory>,
) -> Result<StatusCode, AppError> {
	crate::log_debug!("{addr} PUT /api/categories/{id}");
	let id = validate_id(id)?;
	let name = body.name.trim();
	if name.is_empty() {
		return Err(AppError::invalid_name("category name cannot be empty"));
	}
	let db = state.db.clone();
	let db2 = db.clone();
	let is_default =
		tokio::task::spawn_blocking(move || cat_db::is_default(&db.reader(), id)).await??;
	if is_default {
		return Err(AppError::invalid_name(
			"the default category cannot be renamed",
		));
	}
	let name = name.to_string();
	let renamed =
		tokio::task::spawn_blocking(move || cat_db::rename(&db2.writer(), id, &name)).await??;
	if renamed {
		crate::log_info!("{addr} renamed category #{id} -> {:?}", body.name);
		state.invalidate_caches();
		Ok(StatusCode::OK)
	} else {
		Err(AppError::not_found("category not found"))
	}
}

/// Delete a category. Bookmarks in it move to the default category.
#[utoipa::path(
	delete,
	path = "/categories/{id}",
	tag = "categories",
	params(("id" = i64, Path, description = "Category id")),
	responses(
		(status = 204, description = "Category deleted"),
		(status = 400, description = "Cannot delete default category", body = ApiErrorBody),
		(status = 404, description = "Category not found", body = ApiErrorBody),
	)
)]
pub async fn delete_category(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
	crate::log_debug!("{addr} DELETE /api/categories/{id}");
	let id = validate_id(id)?;
	let db = state.db.clone();
	let db2 = db.clone();
	let is_default =
		tokio::task::spawn_blocking(move || cat_db::is_default(&db.reader(), id)).await??;
	if is_default {
		return Err(AppError::invalid_name(
			"the default category cannot be deleted",
		));
	}
	let deleted = tokio::task::spawn_blocking(move || cat_db::delete(&db2.writer(), id)).await??;
	if deleted {
		crate::log_info!("{addr} deleted category #{id} (bookmarks moved to default)");
		state.invalidate_caches();
		Ok(StatusCode::NO_CONTENT)
	} else {
		Err(AppError::not_found("category not found"))
	}
}

/// List tags with their bookmark counts.
#[utoipa::path(
	get,
	path = "/tags",
	tag = "tags",
	responses((status = 200, description = "Tags with bookmark counts", body = [TagCount])),
)]
pub async fn list_tags(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	headers: HeaderMap,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /api/tags");
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	cached_json(&state, "tags:all".to_string(), if_none_match, move |conn| {
		tag_db::list_with_counts(conn, None, 0)
	})
	.await
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RenameTag {
	name: String,
}

/// Rename a tag. All bookmark associations move with it.
#[utoipa::path(
	put,
	path = "/tags/{old_name}",
	tag = "tags",
	params(("old_name" = String, Path, description = "Current tag name")),
	request_body = RenameTag,
	responses(
		(status = 200, description = "Tag renamed"),
		(status = 400, description = "Invalid name", body = ApiErrorBody),
		(status = 404, description = "Tag not found", body = ApiErrorBody),
	)
)]
pub async fn rename_tag(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(old_name): Path<String>,
	Json(body): Json<RenameTag>,
) -> Result<StatusCode, AppError> {
	crate::log_debug!("{addr} PUT /api/tags/{}", old_name);
	if body.name.trim().is_empty() {
		return Err(AppError::invalid_name("tag name cannot be empty"));
	}
	let db = state.db.clone();
	let old = old_name.clone();
	let new = body.name.clone();
	let renamed =
		tokio::task::spawn_blocking(move || tag_db::rename(&db.writer(), &old, &new)).await??;
	if renamed {
		crate::log_info!("{addr} renamed tag {old_name:?} -> {:?}", body.name);
		state.invalidate_caches();
		Ok(StatusCode::OK)
	} else {
		Err(AppError::not_found("tag not found"))
	}
}

/// Delete a tag. All bookmark-tag associations for it are removed.
#[utoipa::path(
	delete,
	path = "/tags/{name}",
	tag = "tags",
	params(("name" = String, Path, description = "Tag name")),
	responses(
		(status = 204, description = "Tag deleted"),
		(status = 404, description = "Tag not found", body = ApiErrorBody),
	)
)]
pub async fn delete_tag(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
	crate::log_debug!("{addr} DELETE /api/tags/{}", name);
	let db = state.db.clone();
	let tag_name = name.clone();
	let deleted =
		tokio::task::spawn_blocking(move || tag_db::delete(&db.writer(), &tag_name)).await??;
	if deleted {
		crate::log_info!("{addr} deleted tag {name:?}");
		state.invalidate_caches();
		Ok(StatusCode::NO_CONTENT)
	} else {
		Err(AppError::not_found("tag not found"))
	}
}

#[derive(Deserialize, IntoParams)]
pub struct SearchQuery {
	/// The text to search for (matches title, description, note and URL).
	q: Option<String>,
	/// Narrow results to this category.
	category: Option<String>,
	/// Narrow results to bookmarks carrying this tag.
	tag: Option<String>,
	/// Narrow results to this keyword shortcut.
	keyword: Option<String>,
	/// Maximum number of results (1–1000, default 50).
	limit: Option<i64>,
	/// Search the archive index instead of the main (active) one.
	archived: Option<bool>,
}

/// Full-text search. Hits the active index by default; `archived=true`
/// searches the separate archive index.
#[utoipa::path(
	get,
	path = "/search",
	tag = "search",
	params(SearchQuery),
	responses(
		(
			status = 200,
			description = "Search results (active index, or archive index when `archived=true`)",
			body = [Bookmark],
			headers(("x-total-count" = i64, description = "Total matching bookmarks")),
		),
		(status = 400, description = "Missing or invalid query", body = ApiErrorBody),
	)
)]
pub async fn search_bookmarks(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Query(q): Query<SearchQuery>,
) -> Result<Response, AppError> {
	// A missing/blank `q` is a 400, never a "search everything".
	let query = q.q.as_deref().map(str::trim).unwrap_or("");
	if query.is_empty() {
		return Err(AppError::query_required(
			"q is required (the text to search for)",
		));
	}
	let limit = match q.limit {
		None => 50,
		Some(l) if (1..=MAX_PAGE_SIZE).contains(&l) => l,
		Some(l) => {
			return Err(AppError::invalid_limit(format!(
				"limit must be between 1 and {MAX_PAGE_SIZE}, got {l}"
			)));
		}
	};
	let archived = q.archived.unwrap_or(false);
	let filter = BookmarkFilter {
		category: q.category,
		tag: q.tag,
		keyword: q.keyword,
		..Default::default()
	};
	let query = query.to_string();
	crate::log_debug!(
		"{addr} GET /api/search?q={}{}",
		query,
		if archived { "&archived=true" } else { "" }
	);
	let db = state.db.clone();
	let query_for_db = query.clone();
	// Same pattern as list: results + exact total in one spawn_blocking,
	// `count_search` reading the same index as the search itself.
	let (bookmarks, total) = tokio::task::spawn_blocking(move || {
		let conn = db.reader();
		let list = if archived {
			bm_db::search_archived(&conn, &query_for_db, limit, &filter)?
		} else {
			bm_db::search(&conn, &query_for_db, limit, &filter)?
		};
		let total = bm_db::count_search(&conn, &query_for_db, archived, &filter)?;
		Ok::<_, anyhow::Error>((list, total))
	})
	.await??;
	crate::log_debug!(
		"{addr} search for \"{query}\"{} returned {} of {} results",
		if archived { " (archive)" } else { "" },
		bookmarks.len(),
		total
	);
	Ok(([(X_TOTAL_COUNT, total.to_string())], Json(bookmarks)).into_response())
}

/// Top domains by bookmark count.
#[utoipa::path(
	get,
	path = "/stats/domains",
	tag = "stats",
	params(StatsQuery),
	responses((status = 200, description = "Top domains by bookmark count", body = [DomainCount])),
)]
pub async fn domain_stats(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Query(q): Query<StatsQuery>,
	headers: HeaderMap,
) -> Result<Response, AppError> {
	let limit = validate_stats_limit(q.limit, 50)?;
	let offset = validate_offset(q.offset)?;
	crate::log_debug!("{addr} GET /api/stats/domains?limit={limit}&offset={offset}");
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	cached_json(
		&state,
		stats_key("domains", limit, offset),
		if_none_match,
		move |conn| vis_db::domain_counts(conn, limit as usize, offset as usize),
	)
	.await
}

/// Aggregate statistics dashboard: totals, category breakdown, top
/// domains/tags, most-visited, and recently-added bookmarks.
#[utoipa::path(
	get,
	path = "/stats",
	tag = "stats",
	responses((status = 200, description = "Aggregate statistics overview", body = StatsOverview)),
)]
pub async fn stats_overview(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	headers: HeaderMap,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /api/stats");
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	cached_json(
		&state,
		"overview".to_string(),
		if_none_match,
		st_db::overview,
	)
	.await
}

/// Detailed info for a specific bookmark by ID.
#[utoipa::path(
	get,
	path = "/stats/bookmarks/{id}",
	tag = "stats",
	params(("id" = i64, Path, description = "Bookmark ID")),
	responses(
		(status = 200, description = "Bookmark detail", body = Bookmark),
		(status = 400, description = "Invalid bookmark ID", body = ApiErrorBody),
		(status = 404, description = "Bookmark not found", body = ApiErrorBody),
	),
)]
pub async fn stats_bookmark_detail(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<i64>,
) -> Result<Json<Bookmark>, AppError> {
	let id = validate_id(id)?;
	crate::log_debug!("{addr} GET /api/stats/bookmarks/{id}");
	let db = state.db.clone();
	let bookmarks =
		tokio::task::spawn_blocking(move || bm_db::get_by_ids(&db.reader(), &[id])).await??;
	match bookmarks.into_iter().next() {
		Some(b) => Ok(Json(b)),
		None => Err(AppError::not_found("bookmark not found")),
	}
}

/// Top tags by bookmark count.
#[utoipa::path(
	get,
	path = "/stats/tags",
	tag = "stats",
	params(StatsQuery),
	responses((status = 200, description = "Tags with bookmark counts", body = [TagCount])),
)]
pub async fn stats_tags(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Query(q): Query<StatsQuery>,
	headers: HeaderMap,
) -> Result<Response, AppError> {
	let limit = validate_stats_limit(q.limit, 50)?;
	let offset = validate_offset(q.offset)?;
	crate::log_debug!("{addr} GET /api/stats/tags?limit={limit}&offset={offset}");
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	cached_json(
		&state,
		stats_key("tags", limit, offset),
		if_none_match,
		move |conn| tag_db::list_with_counts(conn, Some(limit as usize), offset as usize),
	)
	.await
}

/// Most-visited domains ranked by total visit count across all bookmarks.
#[utoipa::path(
	get,
	path = "/stats/top-visited",
	tag = "stats",
	params(StatsQuery),
	responses((status = 200, description = "Most-visited domains by aggregate visit count", body = [DomainVisitStats])),
)]
pub async fn stats_top_visited(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Query(q): Query<StatsQuery>,
	headers: HeaderMap,
) -> Result<Response, AppError> {
	let limit = validate_stats_limit(q.limit, 20)?;
	let offset = validate_offset(q.offset)?;
	crate::log_debug!("{addr} GET /api/stats/top-visited?limit={limit}&offset={offset}");
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	cached_json(
		&state,
		stats_key("top-visited", limit, offset),
		if_none_match,
		move |conn| vis_db::top_visited_domains(conn, limit as usize, offset as usize),
	)
	.await
}

/// Bookmarks that have never been visited via a keyword shortcut.
#[utoipa::path(
	get,
	path = "/stats/never-visited",
	tag = "stats",
	params(StatsQuery),
	responses((status = 200, description = "Never-visited bookmarks", body = [NeverVisitedBookmark])),
)]
pub async fn stats_never_visited(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Query(q): Query<StatsQuery>,
	headers: HeaderMap,
) -> Result<Response, AppError> {
	let limit = validate_stats_limit(q.limit, 50)?;
	let offset = validate_offset(q.offset)?;
	crate::log_debug!("{addr} GET /api/stats/never-visited?limit={limit}&offset={offset}");
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	cached_json(
		&state,
		stats_key("never-visited", limit, offset),
		if_none_match,
		move |conn| vis_db::never_visited(conn, limit as usize, offset as usize),
	)
	.await
}

/// Tags that are applied to only one bookmark.
#[utoipa::path(
	get,
	path = "/stats/orphan-tags",
	tag = "stats",
	params(StatsQuery),
	responses((status = 200, description = "Orphan tags (used on only 1 bookmark)", body = [OrphanTag])),
)]
pub async fn stats_orphan_tags(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Query(q): Query<StatsQuery>,
	headers: HeaderMap,
) -> Result<Response, AppError> {
	let limit = validate_stats_limit(q.limit, 50)?;
	let offset = validate_offset(q.offset)?;
	crate::log_debug!("{addr} GET /api/stats/orphan-tags?limit={limit}&offset={offset}");
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	cached_json(
		&state,
		stats_key("orphan-tags", limit, offset),
		if_none_match,
		move |conn| tag_db::orphan_tags(conn, limit as usize, offset as usize),
	)
	.await
}

/// How many bookmarks are missing tags, notes, or descriptions.
#[utoipa::path(
	get,
	path = "/stats/hygiene",
	tag = "stats",
	responses((status = 200, description = "Bookmark hygiene stats", body = HygieneStats)),
)]
pub async fn stats_hygiene(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	headers: HeaderMap,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /api/stats/hygiene");
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	cached_json(&state, "hygiene".to_string(), if_none_match, st_db::hygiene).await
}

/// Bookmarks added per month over the last 12 months.
#[utoipa::path(
	get,
	path = "/stats/activity",
	tag = "stats",
	params(StatsQuery),
	responses((status = 200, description = "Monthly activity trend", body = [MonthlyActivity])),
)]
pub async fn stats_activity(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Query(q): Query<StatsQuery>,
	headers: HeaderMap,
) -> Result<Response, AppError> {
	let limit = validate_stats_limit(q.limit, 12)?;
	let offset = validate_offset(q.offset)?;
	crate::log_debug!("{addr} GET /api/stats/activity?limit={limit}&offset={offset}");
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	cached_json(
		&state,
		stats_key("activity", limit, offset),
		if_none_match,
		move |conn| st_db::monthly_activity(conn, limit as usize, offset as usize),
	)
	.await
}

// ============================================================
// Keyword redirect (public — no auth, opened from a browser bar)
// ============================================================

/// List keyword shortcuts as newline-separated plain text.
#[utoipa::path(
	get,
	path = "/keywords",
	tag = "keywords",
	security(()),
	responses((status = 200, description = "Newline-separated list of keyword shortcuts")),
)]
pub async fn keyword_list(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /keywords");
	let filter = BookmarkFilter {
		archived: Some(false),
		trash: false,
		limit: Some(100_000),
		offset: None,
		..Default::default()
	};
	let db = state.db.clone();
	let bookmarks = tokio::task::spawn_blocking(move || {
		let conn = db.reader();
		bm_db::list_keywords(&conn, &filter)
	})
	.await??;
	crate::log_info!("{addr} listed {} keywords", bookmarks.len());
	let body = bookmarks
		.iter()
		.map(|b| b.keyword.as_deref().unwrap_or_default())
		.collect::<Vec<_>>()
		.join("\n");
	Ok((
		[(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
		format!("{body}\n"),
	)
		.into_response())
}

/// Redirect a keyword shortcut to its bookmark.
#[utoipa::path(
	get,
	path = "/keywords/{keyword}",
	tag = "keywords",
	security(()),
	params(("keyword" = String, Path, description = "Keyword shortcut")),
	responses(
		(status = 307, description = "Redirect to the bookmark URL"),
		(status = 404, description = "No bookmark has this keyword"),
	)
)]
pub async fn keyword_redirect(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(keyword): Path<String>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /keywords/{keyword}");
	let db = state.db.clone();
	let lookup_keyword = keyword.clone();
	let bookmark =
		tokio::task::spawn_blocking(move || bm_db::get_by_keyword(&db.reader(), &lookup_keyword))
			.await??;

	match bookmark {
		Some(b) => {
			crate::log_info!(
				"{addr} keyword \"{keyword}\" → bookmark #{id} ({url})",
				id = b.id,
				url = b.url
			);
			// Best-effort visit tracking: fire and forget so a slow or
			// failed write never delays the redirect the user is waiting on.
			// A successful visit touches every aggregate, so invalidate both
			// caches once it lands.
			let db = state.db.clone();
			let state = state.clone();
			let id = b.id;
			tokio::task::spawn_blocking(move || {
				if vis_db::record(&db.writer(), id).is_ok() {
					state.invalidate_caches();
				}
			});
			// 307 Temporary Redirect: unlike 302 it preserves the request
			// method and body across the hop, which is the correct semantic
			// for an address-bar shortcut.
			Ok(Redirect::temporary(&b.url).into_response())
		}
		None => {
			// A missing keyword is a plain 404 with a text body — this
			// route is outside `/api` and returns text, not the JSON error
			// contract.
			crate::log_warn!("{addr} no bookmark for keyword \"{keyword}\"");
			Ok((
				StatusCode::NOT_FOUND,
				format!("no bookmark for keyword \"{keyword}\"\n"),
			)
				.into_response())
		}
	}
}

/// Record a visit and redirect to a bookmark by id. The public
/// address-bar twin of `/keywords/{keyword}`: the frontend card titles
/// point here so opening a bookmark from the UI counts as a visit even
/// without a keyword shortcut.
#[utoipa::path(
	get,
	path = "/open/{id}",
	tag = "keywords",
	security(()),
	params(("id" = i64, Path, description = "Bookmark id")),
	responses(
		(status = 307, description = "Redirect to the bookmark URL"),
		(status = 404, description = "No bookmark has this id"),
	)
)]
pub async fn open_bookmark(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<i64>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /open/{id}");
	let db = state.db.clone();
	let bookmark = tokio::task::spawn_blocking(move || bm_db::get(&db.reader(), id)).await??;

	match bookmark {
		Some(b) => {
			crate::log_info!("{addr} open bookmark #{id} ({url})", id = b.id, url = b.url);
			// Best-effort visit tracking: fire and forget so a slow or
			// failed write never delays the redirect the user is waiting on.
			// A successful visit touches every aggregate, so invalidate both
			// caches once it lands.
			let db = state.db.clone();
			let state = state.clone();
			let id = b.id;
			tokio::task::spawn_blocking(move || {
				if vis_db::record(&db.writer(), id).is_ok() {
					state.invalidate_caches();
				}
			});
			Ok(Redirect::temporary(&b.url).into_response())
		}
		None => {
			// A missing id is a plain 404 with a text body — this route is
			// outside `/api` and returns text, not the JSON error contract.
			crate::log_warn!("{addr} no bookmark with id #{id}");
			Ok((
				StatusCode::NOT_FOUND,
				format!("no bookmark with id #{id}\n"),
			)
				.into_response())
		}
	}
}

// ============================================================
// Embedded / static frontend
// ============================================================

#[derive(Embed)]
#[folder = "frontend/dist/"]
struct Assets;

/// Serves the frontend. Route order means this is the fallback for anything
/// not matched by `/api` or `/keywords` — including `/` itself.
/// Fallback for the `/api` nest: an unmatched JSON route is a 404 in the
/// same `{"error", "code"}` contract as every other API error, not the SPA
/// fallback (which would hand the frontend an HTML document it can't parse).
pub async fn api_404() -> AppError {
	AppError::not_found("no such API endpoint")
}

pub async fn static_handler(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	uri: Uri,
) -> Response {
	let mut path = uri.path().trim_start_matches('/').to_string();
	// `/` maps to the app's entry file.
	if path.is_empty() {
		path = "index.html".to_string();
	}
	crate::log_debug!("{addr} GET /{path} (static)");
	serve_asset(&state, &path).await
}

/// Rejects any static path that could escape its root. A request path is
/// made of components separated by `/`; each is percent-decoded and must not
/// be `.` / `..` / empty (an encoded `%2e%2e` counts too) and must not
/// contain a backslash (a path separator on Windows) or a NUL byte. Returns
/// the sanitized relative path, or `None` for input that must not touch the
/// filesystem.
fn sanitize_static_path(path: &str) -> Option<String> {
	let mut clean = Vec::new();
	for raw in path.split('/') {
		let decoded = percent_decode_str(raw).decode_utf8().ok()?;
		if decoded.is_empty() {
			continue;
		}
		if decoded == "." || decoded == ".." {
			return None;
		}
		// A decoded separator (`%2f` → `/`, `%5c` → `\`) would smuggle a
		// component boundary past the split above; refuse any component that
		// decodes to one, plus NUL.
		if decoded.contains(['/', '\\', '\0']) {
			return None;
		}
		clean.push(decoded);
	}
	let joined = clean.join("/");
	(!joined.is_empty()).then_some(joined)
}

/// Reads one frontend asset from either the on-disk override directory
/// (debug builds only) or the embedded copy. Unknown paths fall back to
/// `index.html` so client-side routing always boots the SPA, and a
/// genuinely missing file (also missing index) becomes a 404.
async fn serve_asset(state: &AppState, path: &str) -> Response {
	// A traversal attempt (`..`, a `\` component, or their percent-encoded
	// forms) is refused outright — it must never fall through to `index.html`
	// or reach `dir.join`.
	let Some(path) = sanitize_static_path(path) else {
		crate::log_warn!("static path traversal attempt refused: {path:?}");
		return (StatusCode::NOT_FOUND, "not found").into_response();
	};
	// Debug override: `--static-dir` serves a frontend build (e.g.
	// `frontend/dist/`) straight off disk so you can re-run `bun build`
	// and reload without recompiling the binary. Release binaries never
	// reach this branch because the flag (and thus `static_dir`) doesn't
	// exist there.
	if let Some(dir) = &state.static_dir {
		if let Ok(bytes) = tokio::fs::read(dir.join(&path)).await {
			let mime = mime_guess::from_path(&path).first_or_octet_stream();
			crate::log_trace!("static: served {path:?} from {} (disk)", dir.display());
			return raw_response(StatusCode::OK, mime.as_ref(), Body::from(bytes));
		}
		if let Ok(bytes) = tokio::fs::read(dir.join("index.html")).await {
			crate::log_trace!("static: {path:?} missing, served fallback index.html (disk)");
			return raw_response(StatusCode::OK, "text/html", Body::from(bytes));
		}
		crate::log_trace!("static: {path:?} not found in {}", dir.display());
		return (StatusCode::NOT_FOUND, "not found").into_response();
	}

	// Embedded copy (release builds): rust-embed keeps the data as `'static`
	// borrowed bytes when possible, which avoids a copy; owned data is only
	// produced for generated files. `Body::from_static` needs a `&'static
	// [u8]`, hence the borrow/owned split.
	match Assets::get(&path) {
		Some(file) => {
			crate::log_trace!("static: served {path:?} (embedded)");
			let mime = mime_guess::from_path(path).first_or_octet_stream();
			let data = match file.data {
				Cow::Borrowed(b) => b,
				Cow::Owned(v) => return raw_response(StatusCode::OK, mime.as_ref(), Body::from(v)),
			};
			raw_response(
				StatusCode::OK,
				mime.as_ref(),
				Body::from(Bytes::from_static(data)),
			)
		}
		None => match Assets::get("index.html") {
			Some(file) => {
				crate::log_trace!(
					"static: {path:?} missing, served fallback index.html (embedded)"
				);
				let data = match file.data {
					Cow::Borrowed(b) => b,
					Cow::Owned(v) => {
						return raw_response(StatusCode::OK, "text/html", Body::from(v));
					}
				};
				raw_response(
					StatusCode::OK,
					"text/html",
					Body::from(Bytes::from_static(data)),
				)
			}
			None => {
				crate::log_trace!("static: {path:?} not found (no embedded index.html)");
				(StatusCode::NOT_FOUND, "not found").into_response()
			}
		},
	}
}

fn raw_response(status: StatusCode, mime: &str, body: Body) -> Response {
	Response::builder()
		.status(status)
		.header(header::CONTENT_TYPE, mime)
		.body(body)
		.expect("static response headers are always valid")
		.into_response()
}

#[cfg(test)]
mod tests {
	use super::sanitize_static_path;

	#[test]
	fn static_path_keeps_clean_assets() {
		assert_eq!(
			sanitize_static_path("index.html").as_deref(),
			Some("index.html")
		);
		assert_eq!(
			sanitize_static_path("assets/app-abc123.js").as_deref(),
			Some("assets/app-abc123.js")
		);
	}

	// Traversal vectors — literal, percent-encoded, and mixed — all refuse.
	#[test]
	fn static_path_rejects_traversal() {
		for path in [
			"../secret",
			"..",
			"a/../../secret",
			"..%2fsecret",
			"%2e%2e/secret",
			"a/%2e%2e/secret",
			"..\\secret",
			"a\\..\\secret",
			"%2e%2e%5csecret",
			"index.html/../secret",
		] {
			assert_eq!(
				sanitize_static_path(path),
				None,
				"{path:?} should be refused"
			);
		}
	}

	// A leading `/` on the raw path is handled by the caller; the sanitizer
	// itself just skips the empty leading component.
	#[test]
	fn static_path_handles_leading_slash() {
		assert_eq!(
			sanitize_static_path("assets/logo.png").as_deref(),
			Some("assets/logo.png")
		);
	}
}
