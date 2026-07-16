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
