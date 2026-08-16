/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Session authentication: exchanging a token for an HttpOnly cookie, the
//! unauthenticated auth-state report, and sign-out. Every auth event —
//! accepted, rejected, session closed — is logged at `info`/`warn` with the
//! client's address, so successful and failed sign-ins are easy to audit.

use std::net::SocketAddr;

use axum::{
	Json,
	extract::{ConnectInfo, State},
	http::{HeaderMap, StatusCode, header},
	response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::http::error::{ApiErrorBody, AppError};
use crate::http::{AppState, auth};

// ============================================================
// Session authentication (unauthenticated routes)
// ============================================================

/// Payload for exchanging a token for a session cookie.
#[derive(Deserialize, ToSchema)]
pub struct SignInRequest {
	/// The token to validate: `WAYPOINTD_SERVE_TOKEN` (full access) or
	/// `WAYPOINTD_READ_TOKEN` (read-only).
	pub token: String,
}

/// Response to a sign-in attempt.
#[derive(Serialize, ToSchema)]
pub struct SignInResponse {
	/// Whether auth is enabled at all on this server.
	pub auth_enabled: bool,
	/// Whether the request is now authenticated (the session cookie is set).
	pub authenticated: bool,
	/// Whether the accepted token only grants read access.
	pub read_only: bool,
}

/// The unauthenticated auth-state report the frontend uses to decide
/// whether to show the sign-in form.
#[derive(Serialize, ToSchema)]
pub struct AuthStatus {
	pub auth_enabled: bool,
	pub authenticated: bool,
	pub read_only: bool,
}

/// The Set-Cookie value for a session cookie carrying `token`. `HttpOnly`
/// keeps the token out of JavaScript; `SameSite=Lax` blocks cross-site
/// request forgery; `Secure` is only added when serving over TLS (the
/// common self-hosted shape is plain HTTP).
fn session_cookie(state: &AppState, token: &str) -> String {
	let mut value = format!(
		"{}={token}; Path=/; HttpOnly; SameSite=Lax",
		auth::SESSION_COOKIE
	);
	if state.cookie_secure {
		value.push_str("; Secure");
	}
	value
}

/// Exchanges a valid token for an HttpOnly session cookie. The cookie's
/// value is the raw token, so rotating the `WAYPOINTD_*` env tokens
/// invalidates every outstanding session immediately — there is no separate
/// session store to flush.
#[utoipa::path(
	post,
	path = "/api/auth/signin",
	tag = "auth",
	request_body = SignInRequest,
	responses(
		(status = 200, description = "Token accepted and session cookie set", body = SignInResponse),
		(status = 401, description = "Invalid token", body = ApiErrorBody),
	)
)]
pub async fn sign_in(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Json(req): Json<SignInRequest>,
) -> Result<Response, AppError> {
	// Auth disabled: there is nothing to sign in to; the API is open.
	if state.api_token.is_none() {
		crate::log_info!("auth: sign-in from {addr} bypassed (no token configured)");
		return Ok(Json(SignInResponse {
			auth_enabled: false,
			authenticated: true,
			read_only: false,
		})
		.into_response());
	}
	// Per-IP breaker: too many failed exchanges lock the address out for a
	// cooldown (see `http::throttle`). Checked before the token is even
	// touched, so a locked-out client can't burn any work.
	if state.login_throttle.locked(addr.ip()) {
		crate::log_warn!("auth: sign-in throttled from {addr}: too many failed attempts");
		return Err(AppError::rate_limited(
			"too many failed sign-in attempts from this address; try again later",
		)
		.already_logged());
	}
	// A failed sign-in gets its own warn line with the client address (the
	// generic `AppError` line is suppressed via `already_logged`), so a
	// brute-force or credential-stuffing attempt is easy to grep for.
	let Some(scope) = auth::classify(&state, &req.token) else {
		crate::log_warn!("auth: sign-in rejected from {addr}: invalid token");
		let locked = state.login_throttle.record_failure(addr.ip());
		if locked {
			crate::log_warn!("auth: {addr} locked out after too many failed sign-in attempts");
		}
		return Err(AppError::unauthorized(
			"that token is not recognized; check WAYPOINTD_SERVE_TOKEN",
		)
		.already_logged());
	};
	state.login_throttle.record_success(addr.ip());
	crate::log_info!("auth: sign-in accepted from {addr} ({})", scope_desc(scope));
	let cookie = session_cookie(&state, &req.token);
	Ok((
		[(header::SET_COOKIE, cookie)],
		Json(SignInResponse {
			auth_enabled: true,
			authenticated: true,
			read_only: scope.is_read_only(),
		}),
	)
		.into_response())
}

fn scope_desc(scope: auth::Scope) -> &'static str {
	if scope.is_read_only() {
		"read-only"
	} else {
		"full"
	}
}

/// Clears the session cookie (browser-side logout). The token isn't
/// invalidated server-side — there's no session store — so a copied cookie
/// value remains valid until the env token rotates, which is the same
/// guarantee bearer tokens have always had.
#[utoipa::path(
	post,
	path = "/api/auth/signout",
	tag = "auth",
	responses((status = 200, description = "Session cookie cleared")),
)]
pub async fn sign_out(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
	crate::log_info!("auth: session closed from {addr}");
	let mut value = format!(
		"{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
		auth::SESSION_COOKIE
	);
	if state.cookie_secure {
		value.push_str("; Secure");
	}
	(StatusCode::OK, [(header::SET_COOKIE, value)], "signed out").into_response()
}

/// Reports whether the current request is authenticated and with what scope.
/// Unauthenticated by design — the frontend calls it on boot to decide
/// whether to show the sign-in form, and it only ever answers yes/no/scope,
/// never token material.
#[utoipa::path(
	get,
	path = "/api/auth/status",
	tag = "auth",
	responses((status = 200, description = "Current auth state", body = AuthStatus)),
)]
pub async fn auth_status(State(state): State<AppState>, headers: HeaderMap) -> Json<AuthStatus> {
	if state.api_token.is_none() {
		return Json(AuthStatus {
			auth_enabled: false,
			authenticated: true,
			read_only: false,
		});
	}
	let scope = auth::authenticate_request(&state, &headers);
	Json(AuthStatus {
		auth_enabled: true,
		authenticated: scope.is_some(),
		read_only: scope.is_some_and(|s| s.is_read_only()),
	})
}
