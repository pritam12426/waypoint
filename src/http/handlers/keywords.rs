/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Keyword-shortcut handlers. These routes are public (no auth) — they're
//! what a browser bar hits. `keyword_list` and `keyword_redirect` live on
//! the unauthenticated router; `open_bookmark` is the UI's visit-counting
//! redirect.

use std::net::SocketAddr;

use axum::{
	extract::{ConnectInfo, Path, State},
	http::{StatusCode, header},
	response::{IntoResponse, Redirect, Response},
};

use crate::http::error::AppError;
use crate::{
	database::{bookmarks as bm_db, visits as vis_db},
	http::AppState,
	model::BookmarkFilter,
};

// ============================================================
// Keyword redirect (public — no auth, opened from a browser bar)
// ============================================================

/// List keyword shortcuts as newline-separated plain text.
#[utoipa::path(
	get,
	path = "/keywords",
	tag = "keywords",
	security(()),
	responses((status = 200, description = "Newline-separated list of keyword shortcuts")),
)]
pub async fn keyword_list(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /keywords");
	let filter = BookmarkFilter {
		archived: Some(false),
		trash: false,
		limit: Some(100_000),
		offset: None,
		..Default::default()
	};
	let db = state.db.clone();
	let bookmarks = tokio::task::spawn_blocking(move || {
		let conn = db.reader();
		bm_db::list_keywords(&conn, &filter)
	})
	.await??;
	crate::log_info!("{addr} listed {} keywords", bookmarks.len());
	let body = bookmarks
		.iter()
		.map(|b| b.keyword.as_deref().unwrap_or_default())
		.collect::<Vec<_>>()
		.join("\n");
	Ok((
		[(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
		format!("{body}\n"),
	)
		.into_response())
}

/// Redirect a keyword shortcut to its bookmark.
#[utoipa::path(
	get,
	path = "/keywords/{keyword}",
	tag = "keywords",
	security(()),
	params(("keyword" = String, Path, description = "Keyword shortcut")),
	responses(
		(status = 307, description = "Redirect to the bookmark URL"),
		(status = 404, description = "No bookmark has this keyword"),
	)
)]
pub async fn keyword_redirect(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(keyword): Path<String>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /keywords/{keyword}");
	let db = state.db.clone();
	let lookup_keyword = keyword.clone();
	let bookmark =
		tokio::task::spawn_blocking(move || bm_db::get_by_keyword(&db.reader(), &lookup_keyword))
			.await??;

	match bookmark {
		Some(b) => {
			crate::log_info!(
				"{addr} keyword \"{keyword}\" → bookmark #{id} ({url})",
				id = b.id,
				url = b.url
			);
			// Best-effort visit tracking: fire and forget so a slow or
			// failed write never delays the redirect the user is waiting on.
			// A successful visit touches every aggregate, so invalidate both
			// caches once it lands.
			let db = state.db.clone();
			let state = state.clone();
			let id = b.id;
			tokio::task::spawn_blocking(move || {
				if vis_db::record(&db.writer(), id).is_ok() {
					state.invalidate_caches();
				}
			});
			// 307 Temporary Redirect: unlike 302 it preserves the request
			// method and body across the hop, which is the correct semantic
			// for an address-bar shortcut.
			Ok(Redirect::temporary(&b.url).into_response())
		}
		None => {
			// A missing keyword is a plain 404 with a text body — this
			// route is outside `/api` and returns text, not the JSON error
			// contract.
			crate::log_warn!("{addr} no bookmark for keyword \"{keyword}\"");
			Ok((
				StatusCode::NOT_FOUND,
				format!("no bookmark for keyword \"{keyword}\"\n"),
			)
				.into_response())
		}
	}
}

/// Record a visit and redirect to a bookmark by id. The public
/// address-bar twin of `/keywords/{keyword}`: the frontend card titles
/// point here so opening a bookmark from the UI counts as a visit even
/// without a keyword shortcut.
#[utoipa::path(
	get,
	path = "/open/{id}",
	tag = "keywords",
	security(()),
	params(("id" = i64, Path, description = "Bookmark id")),
	responses(
		(status = 307, description = "Redirect to the bookmark URL"),
		(status = 404, description = "No bookmark has this id"),
	)
)]
pub async fn open_bookmark(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<i64>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /open/{id}");
	let db = state.db.clone();
	let bookmark = tokio::task::spawn_blocking(move || bm_db::get(&db.reader(), id)).await??;

	match bookmark {
		Some(b) => {
			crate::log_info!("{addr} open bookmark #{id} ({url})", id = b.id, url = b.url);
			// Best-effort visit tracking: fire and forget so a slow or
			// failed write never delays the redirect the user is waiting on.
			// A successful visit touches every aggregate, so invalidate both
			// caches once it lands.
			let db = state.db.clone();
			let state = state.clone();
			let id = b.id;
			tokio::task::spawn_blocking(move || {
				if vis_db::record(&db.writer(), id).is_ok() {
					state.invalidate_caches();
				}
			});
			Ok(Redirect::temporary(&b.url).into_response())
		}
		None => {
			// A missing id is a plain 404 with a text body — this route is
			// outside `/api` and returns text, not the JSON error contract.
			crate::log_warn!("{addr} no bookmark with id #{id}");
			Ok((
				StatusCode::NOT_FOUND,
				format!("no bookmark with id #{id}\n"),
			)
				.into_response())
		}
	}
}
