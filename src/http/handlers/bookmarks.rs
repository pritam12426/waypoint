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
