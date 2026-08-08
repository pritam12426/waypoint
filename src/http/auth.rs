/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! HTTP bearer-token authentication middleware. Placed on the `/api`
//! sub-router and the `/api/openapi.json` docs route, never on `/keywords`
//! or the static frontend.

use axum::{
	extract::{Request, State},
	http::header,
	middleware::Next,
	response::Response,
};
use subtle::ConstantTimeEq;

use super::{AppState, error::AppError};

/// Bearer-token gate for the `/api` router and the `/api/openapi.json` docs
/// route. When no token is configured (`AppState::api_token == None`) every
/// request passes through untouched; when one is set,
/// `Authorization: Bearer <token>` must match it. The comparison is
/// constant-time so the token value can't be recovered by timing a
/// response.
///
/// `/keywords` redirects are deliberately not behind this middleware: they
/// are opened from a browser address bar, which can't attach an
/// `Authorization` header.
pub async fn require_api_token(
	State(state): State<AppState>,
	req: Request,
	next: Next,
) -> Result<Response, AppError> {
	let Some(expected) = &state.api_token else {
		// No token configured — pass everything through untouched. This is
		// the personal-tool default and deliberately leaves the API open.
		crate::log_trace!("auth: no API token configured, allowing request");
		return Ok(next.run(req).await);
	};

	let supplied = req
		.headers()
		.get(header::AUTHORIZATION)
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.strip_prefix("Bearer "));

	let authorized = match supplied {
		Some(token) => token.as_bytes().ct_eq(expected.as_bytes()).into(),
		None => false,
	};

	if authorized {
		crate::log_trace!("auth: valid bearer token");
		Ok(next.run(req).await)
	} else {
		// AppError::into_response logs the rejection and adds the
		// `WWW-Authenticate: Bearer` challenge.
		crate::log_debug!("auth: rejecting request (missing or invalid bearer token)");
		Err(AppError::unauthorized("invalid or missing API token"))
	}
}
