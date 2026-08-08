/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! HTTP layer: the axum server, router assembly, bearer-token auth, unified
//! error responses, and the OpenAPI spec. Every handler delegates to
//! `core` / `database` — the same seam the CLI uses — so a behavior changed
//! here can't silently diverge from `waypoint bookmarks ...`.
//!
//! The router is split into `run` (binds a listener) and `app` (builds the
//! `Router` from state) so integration tests can fire requests with
//! `tower::ServiceExt::oneshot` without ever binding a socket.

mod auth;
mod cache;
mod cursor;
mod docs;
mod error;
pub mod handlers;

pub use cache::{CountCache, StatsCache};

use crate::database;
use anyhow::Result;
use axum::Router;
use axum::extract::Request;
use axum::middleware;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use docs::serve_openapi;
use error::X_WAYPOINT_ERROR;

/// Validates a bind host before any listener is created. Accepts a literal
/// IPv4/IPv6 address or an RFC 1123 hostname (e.g. `localhost`). Applies to
/// both the `--host` flag and `WAYPOINT_SERVE_HOST`.
fn validate_host(host: &str) -> Result<()> {
	if host.parse::<std::net::IpAddr>().is_ok() || is_valid_hostname(host) {
		return Ok(());
	}
	anyhow::bail!(
		"invalid host {host:?}: must be an IPv4/IPv6 address or a hostname like \"localhost\""
	)
}

fn is_valid_hostname(host: &str) -> bool {
	// RFC 1123 hostname rules: 1–253 chars total, dot-separated labels of
	// 1–63 alphanumeric-or-hyphen chars, no leading/trailing hyphen, and a
	// single trailing dot is tolerated (fully-qualified form).
	if host.is_empty() || host.len() > 253 {
		return false;
	}
	let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
	labels.iter().all(|label| {
		!label.is_empty()
			&& label.len() <= 63
			&& label
				.bytes()
				.all(|b| b.is_ascii_alphanumeric() || b == b'-')
			&& !label.starts_with('-')
			&& !label.ends_with('-')
	})
}

/// Rejects port 0 so the listener is always bound to a real port. Applies to
/// both the `--port` flag and `WAYPOINT_SERVE_PORT`.
fn validate_port(port: u16) -> Result<()> {
	if port == 0 {
		anyhow::bail!("invalid port {port}: must be between 1 and 65535");
	}
	Ok(())
}

/// Shared server state. `rusqlite::Connection` is `Send` but not `Sync`, so
/// each connection in the `database::Db` pool is wrapped in its own `Mutex`
/// and accessed from `tokio::task::spawn_blocking`. The pool is one writer
/// (all mutations) plus four round-robin readers (list/count/search/stats),
/// so page loads spread across connections while every write serializes —
/// WAL is what lets the two coexist without blocking each other.
///
/// `api_token`: when set, `/api/*` and the docs require
/// `Authorization: Bearer <token>`. `None` leaves everything open.
#[derive(Clone)]
pub struct AppState {
	pub db: Arc<database::Db>,
	pub counts: Arc<cache::CountCache>,
	pub stats: Arc<cache::StatsCache>,
	pub static_dir: Option<PathBuf>,
	pub api_token: Option<String>,
}

impl AppState {
	/// Drop both caches. Called by every write handler — a bookmark, tag,
	/// or category mutation can change any filter's count and any
	/// aggregate, so coarse invalidation is the safe choice.
	pub fn invalidate_caches(&self) {
		self.counts.invalidate();
		self.stats.invalidate();
	}
}

pub async fn run(
	db_path: PathBuf,
	host: &str,
	port: u16,
	static_dir: Option<PathBuf>,
	api_token: Option<String>,
) -> Result<()> {
	// Fail fast on bad bind arguments *before* touching the database,
	// so a typo'd host/port never leaves a half-initialized server.
	validate_host(host)?;
	validate_port(port)?;

	let db = database::Db::open(&db_path)?;
	crate::log_info!("database ready at {}", db_path.display());

	let state = AppState {
		db: Arc::new(db),
		counts: Arc::new(cache::CountCache::new()),
		stats: Arc::new(cache::StatsCache::new()),
		static_dir,
		api_token,
	};

	let auth_enabled = state.api_token.is_some();
	if auth_enabled {
		crate::log_info!(
			"API token enabled: requests to /api/* must send `Authorization: Bearer <token>`"
		);
	}

	let app = app(state);
	let listener = tokio::net::TcpListener::bind((host, port)).await?;
	crate::log_debug!("router assembled, listener bound");
	crate::log_info!("waypoint listening on http://{host}:{port}");
	// `into_make_service_with_connect_info` is what lets handlers read
	// the client's address via `ConnectInfo` — needed for per-request
	// logging and any future rate limiting.
	axum::serve(
		listener,
		app.into_make_service_with_connect_info::<SocketAddr>(),
	)
	.await?;
	crate::log_info!("waypoint server stopped");

	Ok(())
}

/// Logs every failed response the handlers didn't already log.
///
/// Handlers return `Result<_, AppError>`, and `AppError::into_response`
/// logs its own rejection with the error code + message. This middleware
/// picks up the *other* ways a request goes unfulfilled — axum extractor
/// rejections (malformed JSON body, bad query string, unparseable path
/// segment), missing static assets, and anything else that returns a
/// 4xx/5xx without passing through `AppError`. The `x-waypoint-error`
/// header marks responses that were already logged, so nothing is reported
/// twice. 4xx -> warn, 5xx -> error; both are visible at the default
/// `info` serve log level.
async fn log_failures(req: Request, next: Next) -> Response {
	let method = req.method().clone();
	let path = req.uri().path().to_owned();
	let response = next.run(req).await;
	let status = response.status();
	if (status.is_client_error() || status.is_server_error())
		&& !response.headers().contains_key(X_WAYPOINT_ERROR)
	{
		if status.is_server_error() {
			crate::log_error!("{method} {path}: failed with {status}");
		} else {
			crate::log_warn!("{method} {path}: rejected with {status}");
		}
	}
	response
}

/// Builds the full application router for a given state. Split out from
/// `run` so integration tests can inject requests directly via
/// `tower::ServiceExt::oneshot` without binding a listener.
pub fn app(state: AppState) -> Router {
	// The `/api` sub-router carries every JSON endpoint. Auth is applied as
	// a middleware *layer on this sub-router* (not on the whole app), so
	// the `/keywords` redirects and the static frontend stay open while
	// everything JSON requires the token.
	let api = Router::new()
		.route(
			"/bookmarks",
			get(handlers::list_bookmarks)
				.post(handlers::create_bookmark)
				.delete(handlers::bulk_delete_bookmarks)
				.patch(handlers::bulk_update_bookmarks),
		)
		.route(
			"/bookmarks/{id}",
			get(handlers::get_bookmark)
				.put(handlers::update_bookmark)
				.delete(handlers::delete_bookmark),
		)
		.route("/bookmarks/{id}/restore", post(handlers::restore_bookmark))
		.route("/trash", delete(handlers::empty_trash))
		.route("/categories", get(handlers::list_categories))
		.route(
			"/categories/{id}",
			put(handlers::rename_category).delete(handlers::delete_category),
		)
		.route("/tags", get(handlers::list_tags))
		.route(
			"/tags/{name}",
			put(handlers::rename_tag).delete(handlers::delete_tag),
		)
		.route("/search", get(handlers::search_bookmarks))
		.route("/stats", get(handlers::stats_overview))
		.route("/stats/domains", get(handlers::domain_stats))
		.route("/stats/tags", get(handlers::stats_tags))
		.route(
			"/stats/bookmarks/{id}",
			get(handlers::stats_bookmark_detail),
		)
		.route("/stats/top-visited", get(handlers::stats_top_visited))
		.route("/stats/never-visited", get(handlers::stats_never_visited))
		.route("/stats/orphan-tags", get(handlers::stats_orphan_tags))
		.route("/stats/hygiene", get(handlers::stats_hygiene))
		.route("/stats/activity", get(handlers::stats_activity))
		// Unmatched JSON paths are a JSON 404, not the SPA fallback.
		.fallback(handlers::api_404)
		.layer(middleware::from_fn_with_state(
			state.clone(),
			auth::require_api_token,
		));

	// Docs: the raw OpenAPI spec at /api/openapi.json (the interactive
	// Swagger UI was dropped to keep the release binary small; the JSON
	// spec stays for external tooling). Same bearer-token gate as the API.
	let docs = Router::new()
		.route("/api/openapi.json", get(serve_openapi))
		.layer(middleware::from_fn_with_state(
			state.clone(),
			auth::require_api_token,
		));

	// Top-level router: `/keywords` (address-bar redirects, no auth) and the
	// static frontend fallback are outside the `/api` nest, so they are the
	// only routes reachable without a bearer token.
	Router::new()
		.route("/keywords", get(handlers::keyword_list))
		.route("/keywords/{keyword}", get(handlers::keyword_redirect))
		.route("/open/{id}", get(handlers::open_bookmark))
		.nest("/api", api)
		.merge(docs)
		// Everything unmatched — including the frontend root `/` and its
		// assets — falls through to the embedded static file handler.
		.fallback(handlers::static_handler)
		.layer(middleware::from_fn(log_failures))
		.with_state(state)
}
