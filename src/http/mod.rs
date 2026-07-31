/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! HTTP layer: the axum server, router assembly, authentication, unified
//! error responses, operational endpoints, and the OpenAPI spec. Every
//! handler delegates to `core` / `database` — the server is the only front
//! door.
//!
//! The router is split into `run` (binds a listener + spawns background
//! tasks) and `app` (builds the `Router` from state) so integration tests
//! can fire requests with `tower::ServiceExt::oneshot` without ever binding
//! a socket.
//!
//! # Middleware stack
//!
//! The `/api` sub-router is wrapped, outside-in, by:
//!
//! 1. `auth` — bearer/cookie authentication + scope enforcement (cheap 401
//!    rejects never consume a concurrency slot),
//! 2. `request_timeout` — a per-request deadline (default 30s) answered 504,
//! 3. `concurrency_limit` — a semaphore cap (default 64) answered 503 when
//!    saturated,
//! 4. `DefaultBodyLimit` — 10 MiB JSON payload ceiling,
//! 5. `idempotency` — `Idempotency-Key` replay support on mutating methods.
//!
//! The whole app (including the probes and the static frontend) is wrapped
//! in `CatchPanicLayer` (a panicking handler answers 500 instead of killing
//! the worker) and `log_request` (per-request logs + RED metrics). The
//! probes (`/healthz`, `/readyz`, `/metrics`) are deliberately *outside*
//! the timeout/concurrency layers so a saturated server can still answer a
//! load balancer or scrape.

mod auth;
mod cache;
mod cursor;
mod docs;
mod error;
pub mod handlers;
mod idempotency;
mod jobs;
mod metrics;

pub use cache::{CountCache, StatsCache};
pub use idempotency::IdempotencyStore;
pub use jobs::Jobs;
pub use metrics::Metrics;

use crate::database;
use anyhow::Result;
use axum::Router;
use axum::extract::Request;
use axum::extract::State;
use axum::middleware;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tower_http::catch_panic::CatchPanicLayer;

use docs::{serve_docs_ui, serve_openapi};
use error::X_WAYPOINT_ERROR;

/// Ceiling on JSON request bodies (bookmark payloads, imports, checks).
const BODY_LIMIT_BYTES: usize = 10 * 1024 * 1024;

/// Validates a bind host before any listener is created. Accepts a literal
/// IPv4/IPv6 address or an RFC 1123 hostname (e.g. `localhost`). Applies to
/// the `WAYPOINTD_SERVE_HOST` env var.
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
/// the `WAYPOINTD_SERVE_PORT` env var.
fn validate_port(port: u16) -> Result<()> {
	if port == 0 {
		anyhow::bail!("invalid port {port}: must be between 1 and 65535");
	}
	Ok(())
}

/// Automated-backup settings, derived from `WAYPOINTD_BACKUP_*`.
#[derive(Debug, Clone)]
pub struct BackupConfig {
	pub dir: PathBuf,
	pub interval: Duration,
	pub keep: usize,
}

/// Everything `run` needs that isn't already `AppState`. Read from
/// `config` in `main.rs`, so the binary stays a thin env-config shell.
#[derive(Debug, Clone)]
pub struct Settings {
	pub db_path: PathBuf,
	pub host: String,
	pub port: u16,
	pub api_token: Option<String>,
	pub read_token: Option<String>,
	pub cookie_secure: bool,
	pub wal_checkpoint_secs: u64,
	pub backup: Option<BackupConfig>,
	pub request_timeout: Duration,
	pub max_concurrency: usize,
}

/// Shared server state. `rusqlite::Connection` is `Send` but not `Sync`, so
/// each connection in the `database::Db` pool is wrapped in its own `Mutex`
/// and accessed from `tokio::task::spawn_blocking`. The pool is one writer
/// (all mutations) plus four round-robin readers (list/count/search/stats),
/// so page loads spread across connections while every write serializes —
/// WAL is what lets the two coexist without blocking each other.
///
/// `jobs` holds the in-memory background-check registry (see `jobs.rs`).
///
/// `api_token`: when set, `/api/*` and the docs require authentication
/// (bearer token or session cookie). `read_token` grants the same access
/// for GET/HEAD only. `None` leaves everything open.
///
/// `concurrency` is the request-saturation semaphore and `request_timeout`
/// the per-request deadline; both are consumed by the `/api` middleware.
#[derive(Clone)]
pub struct AppState {
	pub db: Arc<database::Db>,
	pub counts: Arc<cache::CountCache>,
	pub stats: Arc<cache::StatsCache>,
	pub jobs: Arc<Jobs>,
	pub api_token: Option<String>,
	pub read_token: Option<String>,
	pub metrics: Arc<Metrics>,
	pub cookie_secure: bool,
	pub backup: Option<BackupConfig>,
	pub idempotency: Arc<IdempotencyStore>,
	pub concurrency: Arc<Semaphore>,
	pub request_timeout: Duration,
}

