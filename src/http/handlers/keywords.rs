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
	model::{Bookmark, BookmarkFilter},
};

// ============================================================
// Keyword redirect (public — no auth, opened from a browser bar)
// ============================================================

/// All active (non-trashed, non-archived) bookmarks that carry a keyword.
async fn keyword_bookmarks(state: &AppState) -> Result<Vec<Bookmark>, AppError> {
	let filter = BookmarkFilter {
		archived: Some(false),
		trash: false,
		limit: Some(100_000),
		offset: None,
		..Default::default()
	};
	let db = state.db.clone();
	Ok(tokio::task::spawn_blocking(move || {
		let conn = db.reader();
		bm_db::list_keywords(&conn, &filter)
	})
	.await??)
}

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
	let bookmarks = keyword_bookmarks(&state).await?;
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
///
/// The path segment may carry a trailing user value (`yt urMOM`, sent by a
/// browser as `/keywords/yt%20urMOM`). When the bookmark has a
/// `redirect_template`, that value fills the template's
/// `{%s}` placeholder (percent-encoded) and the redirect targets the
/// resulting URL; otherwise — no template, no `{%s}`, or no trailing value —
/// the redirect targets the bookmark's plain `url`.
#[utoipa::path(
	get,
	path = "/keywords/{keyword}",
	tag = "keywords",
	security(()),
	params(("keyword" = String, Path, description = "Keyword shortcut; may carry an optional trailing user value (e.g. `yt urMOM`) that fills the bookmark's redirect template")),
	responses(
		(status = 307, description = "Redirect to the bookmark URL (or to the filled redirect template, when one is set and a user value is given)"),
		(status = 404, description = "No bookmark has this keyword"),
	)
)]
pub async fn keyword_redirect(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(keyword): Path<String>,
) -> Result<Response, AppError> {
	crate::log_debug!("{addr} GET /keywords/{keyword}");
	// A browser bar sends the user value as part of the same segment
	// (`yt urMOM` → `/keywords/yt%20urMOM`), decoded back to a space here.
	// Keywords never contain whitespace (`validate_keyword`), so the first
	// whitespace boundary unambiguously splits shortcut from value.
	let (keyword, user_value) = split_keyword_arg(&keyword);
	let db = state.db.clone();
	let lookup_keyword = keyword.clone();
	let bookmark =
		tokio::task::spawn_blocking(move || bm_db::get_by_keyword(&db.reader(), &lookup_keyword))
			.await??;

	match bookmark {
		Some(b) => {
			let redirect_to = match (b.redirect_template.as_deref(), user_value.as_deref()) {
				(Some(template), Some(value)) => {
					apply_redirect_template(template, value).unwrap_or_else(|| b.url.clone())
				}
				_ => b.url.clone(),
			};
			crate::log_info!(
				"{addr} keyword \"{keyword}\" → bookmark #{id} ({url})",
				id = b.id,
				url = redirect_to
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
			Ok(Redirect::temporary(&redirect_to).into_response())
		}
		None => {
			// A missing keyword is a plain 404 with a text body — this
			// route is outside `/api` and returns text, not the JSON error
			// contract. The body lists every known shortcut so a typo in
			// the address bar is self-correcting.
			crate::log_warn!("{addr} no bookmark for keyword \"{keyword}\"");
			let bookmarks = keyword_bookmarks(&state).await?;
			let mut body = format!("no bookmark for keyword \"{keyword}\"\n\nAll keywords\n");
			let width = bookmarks
				.iter()
				.filter_map(|b| b.keyword.as_deref())
				.map(str::len)
				.max()
				.unwrap_or(0);
			for b in &bookmarks {
				let Some(kw) = b.keyword.as_deref() else {
					continue;
				};
				body.push_str(&format!("    {0:<1$}  {2}", kw, width, b.url));
				if let Some(t) = &b.redirect_template {
					body.push_str(&format!("  ({t})"));
				}
				body.push('\n');
			}
			Ok((StatusCode::NOT_FOUND, body).into_response())
		}
	}
}

/// Splits the decoded `/keywords/{keyword}` segment into the shortcut and an
/// optional trailing user value: `yt urMOM` → (`yt`, `Some("urMOM")`),
/// `yt saying hi` → (`yt`, `Some("saying hi")`), `yt` → (`yt`, `None`).
/// A bare segment with no value yields `None`; an all-whitespace segment
/// yields an empty keyword (which matches nothing and 404s downstream).
fn split_keyword_arg(segment: &str) -> (String, Option<String>) {
	match segment.find(char::is_whitespace) {
		Some(idx) => {
			let keyword = segment[..idx].trim().to_string();
			let value = segment[idx..].trim_start().to_string();
			(keyword, (!value.is_empty()).then_some(value))
		}
		None => (segment.trim().to_string(), None),
	}
}

/// Fills the template's `{%s}` placeholder with the user value. The value is
/// percent-encoded (`space` → `%20`, never a literal space) so the result is
/// a valid URL whether the placeholder sits in a path segment or a query
/// string. Returns `None` when the value is empty or the template carries no
/// `{%s}` — the caller then falls back to the plain `url`.
fn apply_redirect_template(template: &str, user_value: &str) -> Option<String> {
	if user_value.is_empty() || !template.contains("{%s}") {
		return None;
	}
	let encoded =
		percent_encoding::utf8_percent_encode(user_value, percent_encoding::NON_ALPHANUMERIC);
	Some(template.replacen("{%s}", &encoded.to_string(), 1))
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
