/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Bookmark CRUD handlers: list/create/read/update/delete, restore, the
//! bulk operations, and emptying the trash.

use std::net::SocketAddr;
use std::sync::Arc;

use rusqlite::Connection;

use axum::{
	Json,
	extract::{ConnectInfo, Path, Query, State},
	http::{HeaderMap, StatusCode, Uri, header},
	response::{IntoResponse, Response},
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::shared::{
	X_NEXT_CURSOR, X_TOTAL_COUNT, parse_bound, validate_bounds, validate_id, validate_keyword,
	validate_limit, validate_offset,
};
use crate::{
	database::bookmarks as bm_db,
	http::{
		AppState, cursor,
		error::{ApiErrorBody, AppError},
	},
	model::{
		Bookmark, BookmarkFilter, BulkRemoveResult, BulkUpdateRequest, BulkUpdateResult,
		NewBookmark, UpdateBookmark,
	},
};

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

/// List bookmarks, optionally filtered.
#[utoipa::path(
	get,
	path = "/api/bookmarks",
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
	headers: HeaderMap,
	uri: Uri,
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
				// Keep a copy of the filter with the entry so a successful
				// write can refresh the count in place (see
				// `cache::CountCache::refresh`).
				let refresh = {
					let refresh_filter = filter.clone();
					Arc::new(move |conn: &Connection| bm_db::count(conn, &refresh_filter))
				};
				counts.put(&count_key, total, refresh);
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

	// Strong ETag over the page's identity (total + each row's id/updated_at)
	// and `Cache-Control: private, no-cache` (private — the list is per-user
	// filtered data; no-cache — it must be revalidated, never served stale).
	// A matching `If-None-Match` short-circuits to 304, still carrying the
	// pagination headers so a client's cursor walk keeps working on 304s.
	let etag = list_etag(total, &bookmarks);
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	let link = next_cursor
		.as_deref()
		.and_then(|next| next_link(&uri, next));

	let mut response_headers = HeaderMap::new();
	response_headers.insert(X_TOTAL_COUNT, total.to_string().parse().unwrap());
	response_headers.insert(header::CACHE_CONTROL, "private, no-cache".parse().unwrap());
	response_headers.insert(header::ETAG, etag.parse().unwrap());
	if let Some(next) = next_cursor {
		response_headers.insert(X_NEXT_CURSOR, next.parse().unwrap());
	}
	if let Some(link) = link {
		response_headers.insert(header::LINK, link.parse().unwrap());
	}

	if if_none_match == Some(etag.as_str()) {
		crate::log_trace!("list etag matched: 304 Not Modified");
		return Ok((StatusCode::NOT_MODIFIED, response_headers).into_response());
	}
	Ok((response_headers, Json(bookmarks)).into_response())
}

/// Deterministic strong ETag for a bookmarks page: total + every row's id
/// and updated_at. Rebuilt per request (the page is already in memory), so
/// any change to a row on the page — or the count — changes the tag and a
/// revalidation fetches fresh data.
pub(super) fn list_etag(total: i64, bookmarks: &[Bookmark]) -> String {
	use std::collections::hash_map::DefaultHasher;
	use std::hash::{Hash, Hasher};
	let mut hasher = DefaultHasher::new();
	total.hash(&mut hasher);
	for b in bookmarks {
		b.id.hash(&mut hasher);
		b.updated_at.hash(&mut hasher);
	}
	format!("\"{:016x}\"", hasher.finish())
}

/// The RFC 5988 `Link: <...>; rel="next"` header for the next page of a
/// bookmarks list. Rebuilds the request's query string with the fresh
/// cursor token (and drops `offset`, which the cursor supersedes) so the
/// link is a verbatim pointer to the next page.
fn next_link(uri: &Uri, next_cursor: &str) -> Option<String> {
	let mut parts: Vec<String> = uri
		.query()
		.unwrap_or("")
		.split('&')
		.filter(|p| !p.is_empty() && !p.starts_with("cursor=") && !p.starts_with("offset="))
		.map(str::to_owned)
		.collect();
	parts.push(format!("cursor={next_cursor}"));
	let query = parts.join("&");
	Some(format!("</api/bookmarks?{query}>; rel=\"next\""))
}

/// Create a new bookmark.
#[utoipa::path(
	post,
	path = "/api/bookmarks",
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
	state.refresh_caches().await;
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
	path = "/api/bookmarks/{id}",
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
	path = "/api/bookmarks/{id}",
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
	// is invalid; a `None` URL means "unchanged".
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
	state.refresh_caches().await;
	// `describe` diffs the pre-update bookmark against the request so the
	// change log shows exactly what moved ("title: A -> B", "starred: no ->
	// yes").
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
	path = "/api/bookmarks/{id}",
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
		state.refresh_caches().await;
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
	path = "/api/bookmarks/{id}/restore",
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
		state.refresh_caches().await;
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
	path = "/api/bookmarks",
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
		state.refresh_caches().await;
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
