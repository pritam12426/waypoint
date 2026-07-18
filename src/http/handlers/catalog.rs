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
