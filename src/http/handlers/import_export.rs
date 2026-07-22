/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Netscape-HTML import and Markdown/CSV export handlers. The dead-link
//! check endpoints live in `check.rs`.

use std::net::SocketAddr;

use axum::{
	Json,
	extract::{ConnectInfo, Query, State},
	http::header,
	response::{IntoResponse, Response},
};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::http::error::{ApiErrorBody, AppError};
use crate::{
	core::import_export::{self, ImportResult},
	http::AppState,
};

// ============================================================
// Import / export
// ============================================================

/// Request body for `POST /api/import` — the Netscape bookmark HTML file's
/// contents, plus optional overrides for every imported bookmark.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
	/// Netscape bookmark HTML file contents (what a browser's
	/// "Export bookmarks" produces).
	content: String,
	/// Tags added to every imported bookmark.
	tags: Option<Vec<String>>,
	/// Category override for every imported bookmark (overrides the file's
	/// `<H3>` folder headings).
	category: Option<String>,
	/// Import straight into the archive instead of the active list.
	archive: Option<bool>,
}

/// Imports bookmarks from Netscape bookmark HTML. Duplicate URLs are
/// skipped (importing the same file twice is a no-op).
#[utoipa::path(
	post,
	path = "/api/import",
	tag = "bookmarks",
	request_body = ImportRequest,
	responses(
		(
			status = 200,
			description = "Import finished",
			body = ImportResult,
		),
		(status = 400, description = "Empty content or invalid payload", body = ApiErrorBody),
	)
)]
pub async fn import_bookmarks(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Json(body): Json<ImportRequest>,
) -> Result<Json<ImportResult>, AppError> {
	if body.content.trim().is_empty() {
		return Err(AppError::invalid_payload("import content cannot be empty"));
	}
	crate::log_debug!("{addr} POST /api/import ({} bytes)", body.content.len());
	let db = state.db.clone();
	let result = tokio::task::spawn_blocking(move || {
		let conn = db.writer();
		import_export::import_html(
			&conn,
			&body.content,
			body.tags,
			body.category,
			body.archive.unwrap_or(false),
		)
	})
	.await??;
	crate::log_info!(
		"{addr} imported {} bookmarks ({} skipped)",
		result.imported,
		result.skipped
	);
	if result.imported > 0 {
		state.refresh_caches().await;
	}
	Ok(Json(result))
}

/// Query parameters for `GET /api/export`.
#[derive(Deserialize, IntoParams)]
pub struct ExportQuery {
	/// Output format: `md` (default) or `csv`.
	format: Option<String>,
}

/// Exports every active bookmark as Markdown or CSV. Only *active*
/// bookmarks are exported — trashed and archived content stays out.
