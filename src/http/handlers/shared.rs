/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Shared handler plumbing: query validation and the cached-JSON/ETag
//! pipeline used by the list, search, and stats endpoints. Everything here
//! is `pub(super)` — visible to the sibling handler modules, never re-exported
//! through `handlers::*`.

use std::sync::Arc;

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
