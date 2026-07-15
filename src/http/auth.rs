/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! API authentication: bearer-token and HttpOnly session-cookie support,
//! with scope enforcement between the full-access and read-only tokens.
//!
//! Placed on the `/api` sub-router (and the `/api/openapi.json` docs
//! route), never on `/keywords`, `/open`, the static frontend, or the
//! `/healthz` / `/readyz` / `/metrics` probes. When no token is configured
//! everything passes through untouched — the personal-tool default.
//!
//! A request authenticates either via `Authorization: Bearer <token>` (API
//! clients, `curl`) or the `waypointd_session` cookie that the
//! `/api/auth/signin` endpoint issues (the browser). Both are checked in
//! constant time against the configured tokens, so the token value can't be
//! recovered by timing a response.
//!
//! Two tokens can be configured:
//!
//! * `WAYPOINTD_SERVE_TOKEN` — full access (everything).
//! * `WAYPOINTD_READ_TOKEN` — read-only access (GET/HEAD only); anything
//!   else is rejected with 403.
//!
//! The read-only token is a convenience for scripts that only want to query
//! without the ability to mutate the library.

use axum::{
	extract::{Request, State},
	http::{Method, header},
	middleware::Next,
	response::Response,
};
use subtle::ConstantTimeEq;

use super::{AppState, error::AppError};

/// Name of the HttpOnly session cookie. The value is the raw accepted token
/// — there is no server-side session table to invalidate, and rotating the
/// `WAYPOINTD_*` token invalidates every outstanding cookie instantly.
/// `HttpOnly` keeps the token out of JavaScript (so an XSS can't exfiltrate
/// it), and `SameSite=Lax` stops cross-site request forgery without the
/// friction of `Strict`.
pub const SESSION_COOKIE: &str = "waypointd_session";

/// What a supplied token is authorized to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
	/// The `WAYPOINTD_SERVE_TOKEN` — every endpoint.
	Full,
	/// The `WAYPOINTD_READ_TOKEN` — GET/HEAD only.
	ReadOnly,
}

impl Scope {
	pub fn is_read_only(self) -> bool {
		self == Scope::ReadOnly
	}

	/// Whether a request with this scope may use `method`.
	fn allows(self, method: &Method) -> bool {
		match self {
			Scope::Full => true,
			Scope::ReadOnly => matches!(method, &Method::GET | &Method::HEAD),
		}
	}
}

/// Constant-time comparison of a supplied token against one expected value.
fn token_matches(supplied: &str, expected: &str) -> bool {
	supplied.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// Classifies a supplied token against the configured tokens. `None` means
/// it matches neither (the request is unauthorized); `Some(scope)` says
/// what it may do.
pub(crate) fn classify(state: &AppState, supplied: &str) -> Option<Scope> {
	if let Some(full) = &state.api_token
		&& token_matches(supplied, full)
	{
		return Some(Scope::Full);
	}
	if let Some(read) = &state.read_token
		&& token_matches(supplied, read)
	{
		return Some(Scope::ReadOnly);
	}
	None
}

/// Extracts the bearer token from `Authorization: Bearer <token>` if the
/// header is present and well-formed.
fn bearer_from_headers(headers: &header::HeaderMap) -> Option<&str> {
	headers
		.get(header::AUTHORIZATION)
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.strip_prefix("Bearer "))
}

/// Extracts the session cookie value, if present.
fn session_cookie_from_headers(headers: &header::HeaderMap) -> Option<String> {
	let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
	for part in cookies.split(';') {
		let part = part.trim();
		if let Some(value) = part.strip_prefix(&format!("{SESSION_COOKIE}=")) {
			return Some(value.to_owned());
		}
	}
	None
}

/// Classifies the credentials on an incoming request (bearer header first,
/// session cookie second) against the configured tokens. `None` when auth is
/// disabled or the request carries no valid token — the caller decides which
/// of those it is.
pub fn authenticate_request(state: &AppState, headers: &header::HeaderMap) -> Option<Scope> {
	if state.api_token.is_none() {
		return Some(Scope::Full);
	}
	let supplied = bearer_from_headers(headers)
		.map(str::to_owned)
		.or_else(|| session_cookie_from_headers(headers));
	supplied.as_deref().and_then(|t| classify(state, t))
}

/// Authentication gate for the `/api` router. Accepts a bearer token or
/// the session cookie, classifies the scope, and rejects:
///
/// * requests with no valid token at all → 401 (with the
///   `WWW-Authenticate: Bearer` challenge),
/// * requests where a read-only token tries a mutating method → 403.
pub async fn require_api_token(
	State(state): State<AppState>,
	req: Request,
	next: Next,
) -> Result<Response, AppError> {
	// Auth is only ever "on" when a full-access token is configured. A
	// stray read-only token with no full token leaves the API open (the
	// documented personal-tool default) — see `run` for the startup warning.
	if state.api_token.is_none() {
		crate::log_trace!("auth: no API token configured, allowing request");
		return Ok(next.run(req).await);
	}

	// Bearer header wins over the cookie when both are present (an API
	// client's explicit credential beats whatever the browser attached).
	let scope = authenticate_request(&state, req.headers());

	let Some(scope) = scope else {
		crate::log_debug!("auth: rejecting request (missing or invalid token)");
		return Err(AppError::unauthorized("invalid or missing API token"));
	};

	if !scope.allows(req.method()) {
		crate::log_debug!("auth: read-only token rejected {} request", req.method());
		return Err(AppError::forbidden(
			"read-only token cannot perform this request",
		));
	}

	crate::log_trace!(
		"auth: valid {} token",
		if scope.is_read_only() {
			"read-only"
		} else {
			"full"
		}
	);
	Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::http::AppState;
	use crate::http::cache::{CountCache, StatsCache};
	use crate::http::idempotency::IdempotencyStore;
	use crate::http::metrics::Metrics;
	use std::sync::Arc;
	use std::time::Duration;

	fn state(full: Option<&str>, read: Option<&str>) -> AppState {
		AppState {
			db: Arc::new(crate::database::Db::in_memory().unwrap()),
			counts: Arc::new(CountCache::new()),
			stats: Arc::new(StatsCache::new()),
			jobs: Arc::new(crate::http::Jobs::new()),
			api_token: full.map(str::to_owned),
			read_token: read.map(str::to_owned),
			metrics: Arc::new(Metrics::new()),
			cookie_secure: false,
			backup: None,
			idempotency: Arc::new(IdempotencyStore::new()),
			concurrency: Arc::new(tokio::sync::Semaphore::new(64)),
			request_timeout: Duration::from_secs(30),
		}
	}

	#[test]
	fn scope_classification() {
		let s = state(Some("full-secret"), Some("read-secret"));
		assert_eq!(classify(&s, "full-secret"), Some(Scope::Full));
		assert_eq!(classify(&s, "read-secret"), Some(Scope::ReadOnly));
		assert_eq!(classify(&s, "wrong"), None);
		// Without a full token the read token still classifies, but `run`
		// never routes requests here in that configuration.
		let only_read = state(None, Some("read-secret"));
		assert_eq!(classify(&only_read, "read-secret"), Some(Scope::ReadOnly));
	}

	#[test]
	fn scope_allows_methods() {
		assert!(Scope::Full.allows(&Method::POST));
		assert!(Scope::ReadOnly.allows(&Method::GET));
		assert!(Scope::ReadOnly.allows(&Method::HEAD));
		assert!(!Scope::ReadOnly.allows(&Method::POST));
		assert!(!Scope::ReadOnly.allows(&Method::DELETE));
		assert!(!Scope::ReadOnly.allows(&Method::PATCH));
	}
}
