/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Category, tag, and search handlers. The stats endpoints live in
//! `stats.rs`; these are the catalog-style lookups and the FTS search.

use std::net::SocketAddr;

use axum::{
	Json,
	extract::{ConnectInfo, Path, Query, State},
	http::{HeaderMap, StatusCode, header},
	response::{IntoResponse, Response},
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::{
	bookmarks::list_etag,
	shared::{X_TOTAL_COUNT, cached_json, validate_id, validate_stats_limit},
};
use crate::{
	database::{bookmarks as bm_db, categories as cat_db, tags as tag_db},
	http::{
		AppState,
		error::{ApiErrorBody, AppError},
	},
	model::{Bookmark, BookmarkFilter, Category, TagCount},
};

// ============================================================
// Categories / tags / search
// ============================================================

/// List all categories.
#[utoipa::path(
	get,
	path = "/api/categories",
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
	path = "/api/categories/{id}",
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
		state.refresh_caches().await;
		Ok(StatusCode::OK)
	} else {
		Err(AppError::not_found("category not found"))
	}
}

/// Delete a category. Bookmarks in it move to the default category.
#[utoipa::path(
	delete,
	path = "/api/categories/{id}",
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
		state.refresh_caches().await;
		Ok(StatusCode::NO_CONTENT)
	} else {
		Err(AppError::not_found("category not found"))
	}
}

/// List tags with their bookmark counts.
#[utoipa::path(
	get,
	path = "/api/tags",
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
	path = "/api/tags/{old_name}",
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
		state.refresh_caches().await;
		Ok(StatusCode::OK)
	} else {
		Err(AppError::not_found("tag not found"))
	}
}

/// Delete a tag. All bookmark-tag associations for it are removed.
#[utoipa::path(
	delete,
	path = "/api/tags/{name}",
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
		state.refresh_caches().await;
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
	path = "/api/search",
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
	headers: HeaderMap,
) -> Result<Response, AppError> {
	// A missing/blank `q` is a 400, never a "search everything".
	let query = q.q.as_deref().map(str::trim).unwrap_or("");
	if query.is_empty() {
		return Err(AppError::query_required(
			"q is required (the text to search for)",
		));
	}
	let limit = validate_stats_limit(q.limit, 50)?;
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
	// Same revalidation semantics as the list: strong ETag + must-revalidate,
	// so a client that already has these exact results gets a cheap 304.
	let etag = list_etag(total, &bookmarks);
	let if_none_match = headers
		.get(header::IF_NONE_MATCH)
		.and_then(|v| v.to_str().ok());
	let mut response_headers = HeaderMap::new();
	response_headers.insert(X_TOTAL_COUNT, total.to_string().parse().unwrap());
	response_headers.insert(header::CACHE_CONTROL, "private, no-cache".parse().unwrap());
	response_headers.insert(header::ETAG, etag.parse().unwrap());
	if if_none_match == Some(etag.as_str()) {
		return Ok((StatusCode::NOT_MODIFIED, response_headers).into_response());
	}
	Ok((response_headers, Json(bookmarks)).into_response())
}
