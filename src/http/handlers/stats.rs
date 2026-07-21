/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Aggregated stats endpoints: domains, tags, activity, hygiene, and the
//! per-bookmark detail. All served through the shared cached-JSON pipeline.

use std::net::SocketAddr;

use axum::{
	Json,
	extract::{ConnectInfo, Path, Query, State},
	http::{HeaderMap, header},
	response::Response,
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::shared::{cached_json, stats_key, validate_id, validate_offset, validate_stats_limit};
use crate::{
	database::{bookmarks as bm_db, stats as st_db, tags as tag_db, visits as vis_db},
	http::{
		AppState,
		error::{ApiErrorBody, AppError},
	},
	model::{
		Bookmark, DomainCount, DomainVisitStats, HygieneStats, MonthlyActivity,
		NeverVisitedBookmark, OrphanTag, StatsOverview, TagCount,
	},
};

/// Pagination shared by the paged stats sub-resources. `limit` defaults
/// differ per endpoint (see each handler); `offset` always defaults to 0.
#[derive(Deserialize, IntoParams)]
pub struct StatsQuery {
	/// Maximum number of results.
	limit: Option<i64>,
	/// Number of results to skip.
	offset: Option<i64>,
}

/// Top domains by bookmark count.
#[utoipa::path(
	get,
	path = "/api/stats/domains",
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
	path = "/api/stats",
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
	path = "/api/stats/bookmarks/{id}",
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
	path = "/api/stats/tags",
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
	path = "/api/stats/top-visited",
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
