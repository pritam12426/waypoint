/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Shared handler plumbing: query validation and the cached-JSON/ETag
//! pipeline used by the list, search, and stats endpoints. Everything here
//! is `pub(super)` — visible to the sibling handler modules, never re-exported
//! through `handlers::*`.

use axum::{
	http::{HeaderName, StatusCode, header},
	response::{IntoResponse, Response},
};
use rusqlite::Connection;

use crate::http::{AppState, error::AppError};
use crate::shared;

// ============================================================
// Shared validation
// ============================================================

/// `X-Total-Count` response header: total matches ignoring pagination.
/// Deliberately lowercase — axum 0.8 does not normalize header names, so a
/// capitalized constant would silently become a second, ignored header.
pub(super) const X_TOTAL_COUNT: HeaderName = HeaderName::from_static("x-total-count");

/// `X-Next-Cursor` response header: opaque keyset token for the next page.
/// Same lowercase rule as `X_TOTAL_COUNT`.
pub(super) const X_NEXT_CURSOR: HeaderName = HeaderName::from_static("x-next-cursor");

/// The validation rules themselves live in `crate::shared` (same shapes the
/// SQL layer and docs use); these thin wrappers translate their `String`
/// messages into a 400 `AppError`.
pub(super) fn validate_limit(limit: Option<i64>) -> Result<i64, AppError> {
	shared::validate_limit(limit).map_err(AppError::invalid_limit)
}

pub(super) fn validate_offset(offset: Option<i64>) -> Result<i64, AppError> {
	shared::validate_offset(offset).map_err(AppError::invalid_offset)
}

/// `limit` for the stats sub-resources. Unlike the list endpoints there is
/// no single natural default (domains: 50, top-visited: 20, activity: 12),
/// so callers pass theirs in; the range and error contract stay identical
/// to `validate_limit`.
pub(super) fn validate_stats_limit(limit: Option<i64>, default: i64) -> Result<i64, AppError> {
	shared::validate_limit_with_default(limit, default).map_err(AppError::invalid_limit)
}

pub(super) fn validate_id(id: i64) -> Result<i64, AppError> {
	shared::validate_id(id).map_err(AppError::invalid_id)
}

/// Keywords become URL path segments at `/keywords/{keyword}`, so they are
/// restricted to the same safe charset a path segment tolerates.
pub(super) fn validate_keyword(keyword: Option<&str>) -> Result<(), AppError> {
	if let Some(k) = keyword
		&& !k.is_empty()
		&& !k
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
	{
		return Err(AppError::invalid_keyword(
			"a keyword may only contain letters, digits, '.', '_' and '-' \
			 (it becomes the /keywords/{keyword} path)",
		));
	}
	Ok(())
}

/// A stored URL must be free of control characters. A URL carrying a CR/LF
/// makes axum's `Redirect::into_response` fail to build a Location header,
/// turning every `/open/{id}` / `/keywords/{k}` redirect into a 500. That is
/// the only gate here: non-http schemes (`mailto:`, `javascript:`, ...) are
/// legitimate bookmarks and stay storable — the dead-link checker skips them
/// via `core::url::is_http_url`, which is what guards redirects to them.
pub(super) fn validate_url(url: &str) -> Result<(), AppError> {
	if url.bytes().any(|b| b.is_ascii_control()) {
		return Err(AppError::invalid_url(
			"the url may not contain control characters",
		));
	}
	Ok(())
}

/// A redirect template must carry at least one `{%s}` placeholder — without
/// one there is nothing for the address-bar value to fill, and the shortcut
/// would silently always fall back to the plain URL. An empty string clears
/// the template, so it is exempt.
pub(super) fn validate_redirect_template(template: Option<&str>) -> Result<(), AppError> {
	if let Some(t) = template
		&& !t.is_empty()
		&& !t.contains("{%s}")
	{
		return Err(AppError::invalid_payload(
			"a redirect template must contain at least one {%s} placeholder",
		));
	}
	// A template is substituted into a redirect Location, so a control
	// character would break the header exactly like a bad stored URL (500 on
	// every hit) — keep it out.
	if let Some(t) = template
		&& t.bytes().any(|b| b.is_ascii_control())
	{
		return Err(AppError::invalid_payload(
			"a redirect template may not contain control characters",
		));
	}
	Ok(())
}

/// Parses an optional `--*-after`/`--*-before` query string into the
/// normalized `YYYY-MM-DD HH:MM:SS` UTC form `BookmarkFilter` expects.
/// `end_of_day` picks day-start for `*_after` bounds and day-end for
/// `*_before` bounds so a bare date is inclusive of the whole day. A bad
/// value is a 400 (`invalid_date`) instead of a SQL error.
pub(super) fn parse_bound(
	value: Option<String>,
	end_of_day: bool,
) -> Result<Option<String>, AppError> {
	shared::parse_datetime_bound_option(value, end_of_day).map_err(AppError::invalid_date)
}

/// Enforces that each normalized `*_after` / `*_before` pair is a sane range
/// (after must not sort after before). The values are already fixed-width
/// `YYYY-MM-DD HH:MM:SS` UTC strings from `parse_bound`, so plain
/// lexicographic comparison is chronological. An inverted range is a 400
/// (`invalid_date`) rather than a silently-empty list.
pub(super) fn validate_bounds(
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
/// (see `cache::Cache`); the key must include pagination because a 50-row
/// slice and a 200-row slice are different queries.
pub(super) fn stats_key(endpoint: &str, limit: i64, offset: i64) -> String {
	format!("{endpoint}:{limit}:{offset}")
}

/// Runs `compute` against the database and serves its JSON through the
/// stats cache (30s TTL). Emits `Cache-Control: private, max-age=30`
/// (matching the server TTL, so the browser can serve the dashboard from
/// its own cache) and a strong ETag; a matching `If-None-Match` short-
/// circuits to 304.
pub(super) async fn cached_json<T>(
	state: &AppState,
	key: String,
	if_none_match: Option<&str>,
	compute: impl Fn(&Connection) -> anyhow::Result<T> + Send + 'static,
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
pub(super) fn etag_response(body: Vec<u8>, if_none_match: Option<&str>) -> Response {
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
pub(super) fn body_etag(body: &[u8]) -> String {
	use std::collections::hash_map::DefaultHasher;
	use std::hash::{Hash, Hasher};
	let mut hasher = DefaultHasher::new();
	body.hash(&mut hasher);
	format!("\"{:016x}\"", hasher.finish())
}
