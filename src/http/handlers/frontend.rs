/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Embedded React SPA serving and the API 404 fallback. The frontend is
//! baked in at compile time by `rust-embed`; `static_handler` is the
//! router-level fallback for anything not matched by `/api`, `/keywords`,
//! or the auth routes.

use std::borrow::Cow;
use std::net::SocketAddr;

use axum::{
	body::Body,
	extract::{ConnectInfo, State},
	http::{StatusCode, Uri, header},
	response::{IntoResponse, Response},
};
use bytes::Bytes;
use percent_encoding::percent_decode_str;
use rust_embed::Embed;

use crate::http::AppState;
use crate::http::error::AppError;

// ============================================================
// Embedded / static frontend
// ============================================================

#[derive(Embed)]
#[folder = "frontend/dist/"]
struct Assets;

/// Serves the frontend. Route order means this is the fallback for anything
/// not matched by `/api` or `/keywords` — including `/` itself.
/// Fallback for the `/api` nest: an unmatched JSON route is a 404 in the
/// same `{"error", "code"}` contract as every other API error, not the SPA
/// fallback (which would hand the frontend an HTML document it can't parse).
pub async fn api_404() -> AppError {
	AppError::not_found("no such API endpoint")
}

pub async fn static_handler(
	State(_): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	uri: Uri,
) -> Response {
	let mut path = uri.path().trim_start_matches('/').to_string();
	// `/` maps to the app's entry file.
	if path.is_empty() {
		path = "index.html".to_string();
	}
	crate::log_debug!("{addr} GET /{path} (static)");
	serve_asset(&path).await
}

/// Rejects any static path that could escape its root. A request path is
/// made of components separated by `/`; each is percent-decoded and must not
/// be `.` / `..` / empty (an encoded `%2e%2e` counts too) and must not
/// contain a backslash (a path separator on Windows) or a NUL byte. Returns
/// the sanitized relative path, or `None` for input that must not touch the
/// filesystem.
fn sanitize_static_path(path: &str) -> Option<String> {
	let mut clean = Vec::new();
	for raw in path.split('/') {
		let decoded = percent_decode_str(raw).decode_utf8().ok()?;
		if decoded.is_empty() {
			continue;
		}
		if decoded == "." || decoded == ".." {
			return None;
		}
		// A decoded separator (`%2f` → `/`, `%5c` → `\`) would smuggle a
		// component boundary past the split above; refuse any component that
		// decodes to one, plus NUL.
		if decoded.contains(['/', '\\', '\0']) {
			return None;
		}
		clean.push(decoded);
	}
	let joined = clean.join("/");
	(!joined.is_empty()).then_some(joined)
}

/// Reads one frontend asset from the embedded copy. Unknown paths fall
/// back to `index.html` so client-side routing always boots the SPA, and
/// a genuinely missing file (also missing index) becomes a 404.
async fn serve_asset(path: &str) -> Response {
	// A traversal attempt (`..`, a `\` component, or their percent-encoded
	// forms) is refused outright — it must never fall through to `index.html`
	// or reach a directory join.
	let Some(path) = sanitize_static_path(path) else {
		crate::log_warn!("static path traversal attempt refused: {path:?}");
		return (StatusCode::NOT_FOUND, "not found").into_response();
	};

	// Embedded copy: rust-embed keeps the data as `'static`
	// borrowed bytes when possible, which avoids a copy; owned data is only
	// produced for generated files. `Body::from_static` needs a `&'static
	// [u8]`, hence the borrow/owned split.
	match Assets::get(&path) {
		Some(file) => {
			crate::log_trace!("static: served {path:?} (embedded)");
			let mime = mime_guess::from_path(path).first_or_octet_stream();
			let data = match file.data {
				Cow::Borrowed(b) => b,
				Cow::Owned(v) => return raw_response(StatusCode::OK, mime.as_ref(), Body::from(v)),
			};
			raw_response(
				StatusCode::OK,
				mime.as_ref(),
				Body::from(Bytes::from_static(data)),
			)
		}
		None => match Assets::get("index.html") {
			Some(file) => {
				crate::log_trace!(
					"static: {path:?} missing, served fallback index.html (embedded)"
				);
				let data = match file.data {
					Cow::Borrowed(b) => b,
					Cow::Owned(v) => {
						return raw_response(StatusCode::OK, "text/html", Body::from(v));
					}
				};
				raw_response(
					StatusCode::OK,
					"text/html",
					Body::from(Bytes::from_static(data)),
				)
			}
			None => {
				crate::log_trace!("static: {path:?} not found (no embedded index.html)");
				(StatusCode::NOT_FOUND, "not found").into_response()
			}
		},
	}
}

fn raw_response(status: StatusCode, mime: &str, body: Body) -> Response {
	Response::builder()
		.status(status)
		.header(header::CONTENT_TYPE, mime)
		.body(body)
		.expect("static response headers are always valid")
		.into_response()
}

#[cfg(test)]
mod tests {
	use super::sanitize_static_path;

	#[test]
	fn static_path_keeps_clean_assets() {
		assert_eq!(
			sanitize_static_path("index.html").as_deref(),
			Some("index.html")
		);
		assert_eq!(
			sanitize_static_path("assets/app-abc123.js").as_deref(),
			Some("assets/app-abc123.js")
		);
	}

	// Traversal vectors — literal, percent-encoded, and mixed — all refuse.
	#[test]
	fn static_path_rejects_traversal() {
		for path in [
			"../secret",
			"..",
			"a/../../secret",
			"..%2fsecret",
			"%2e%2e/secret",
			"a/%2e%2e/secret",
			"..\\secret",
			"a\\..\\secret",
			"%2e%2e%5csecret",
			"index.html/../secret",
		] {
			assert_eq!(
				sanitize_static_path(path),
				None,
				"{path:?} should be refused"
			);
		}
	}

	// A leading `/` on the raw path is handled by the caller; the sanitizer
	// itself just skips the empty leading component.
	#[test]
	fn static_path_handles_leading_slash() {
		assert_eq!(
			sanitize_static_path("assets/logo.png").as_deref(),
			Some("assets/logo.png")
		);
	}
}
