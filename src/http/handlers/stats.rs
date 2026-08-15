/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Aggregated stats endpoints: domains, tags, activity, hygiene, and the
//! per-bookmark detail. All served through the shared cached-JSON pipeline.

use std::net::SocketAddr;

use axum::{
	extract::{ConnectInfo, Query, State},
	http::{HeaderMap, header},
	response::Response,
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::shared::{cached_json, stats_key, validate_offset, validate_stats_limit};
use crate::{
	database::{stats as st_db, tags as tag_db, visits as vis_db},
	http::{AppState, error::AppError},
	model::{
		DomainCount, DomainVisitStats, HygieneStats, MonthlyActivity, NeverVisitedBookmark,
		OrphanTag, StatsOverview, TagCount,
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
#[utoipa::path(
	get,
	path = "/api/stats/never-visited",
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
	path = "/api/stats/orphan-tags",
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
	path = "/api/stats/hygiene",
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
	path = "/api/stats/activity",
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
