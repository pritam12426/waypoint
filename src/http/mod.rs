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

impl AppState {
	/// Recompute every warm cache entry in place after a successful write,
	/// so the in-memory caches reflect the new data immediately and the next
	/// read is still a hit. Entries that were never cached stay cold and are
	/// computed on demand as before. Runs the recomputes on a blocking task
	/// (they are SQLite queries); if that task fails the caches are dropped
	/// instead so nothing stale is ever served.
	pub async fn refresh_caches(&self) {
		let counts = self.counts.clone();
		let stats = self.stats.clone();
		let db = self.db.clone();
		let ok = tokio::task::spawn_blocking(move || {
			let conn = db.reader();
			counts.refresh(&conn);
			stats.refresh(&conn);
		})
		.await
		.is_ok();
		if !ok {
			crate::log_warn!("cache refresh panicked — invalidating caches instead");
			self.invalidate_caches();
		}
	}

	/// Drop both caches. The visit-tracking redirects and any cache-refresh
	/// failure land here: coarse invalidation is the safe choice because a
	/// bookmark, tag, or category mutation can change any filter's count and
	/// any aggregate.
	pub fn invalidate_caches(&self) {
		self.counts.invalidate();
		self.stats.invalidate();
	}
}

pub async fn run(settings: Settings) -> Result<()> {
	// Fail fast on bad bind arguments *before* touching the database,
	// so a typo'd host/port never leaves a half-initialized server.
	validate_host(&settings.host)?;
	validate_port(settings.port)?;

	let db = database::Db::open(&settings.db_path)?;
	crate::log_info!("database ready at {}", settings.db_path.display());

	// A clone kept here survives the move into the router below, so the
	// shutdown path and the background maintenance tasks can still reach
	// the pool after the server stops serving.
	let db_arc = Arc::new(db);
	let state = AppState {
		db: db_arc.clone(),
		counts: Arc::new(cache::CountCache::new()),
		stats: Arc::new(cache::StatsCache::new()),
		jobs: Arc::new(Jobs::new()),
		api_token: settings.api_token.clone(),
		read_token: settings.read_token.clone(),
		metrics: Arc::new(Metrics::new()),
		cookie_secure: settings.cookie_secure,
		backup: settings.backup.clone(),
		idempotency: Arc::new(IdempotencyStore::new()),
		concurrency: Arc::new(Semaphore::new(settings.max_concurrency)),
		request_timeout: settings.request_timeout,
	};

	if state.api_token.is_some() {
		crate::log_info!(
			"API token enabled: requests to /api/* must authenticate (bearer or session cookie)"
		);
	}
	if state.api_token.is_none() && state.read_token.is_some() {
		// A read-only token with no full token would be unreachable (auth
		// is only ever "on" when a full token exists) — warn instead of
		// silently ignoring it.
		crate::log_warn!(
			"WAYPOINTD_READ_TOKEN is set but WAYPOINTD_SERVE_TOKEN is not: auth is disabled, the read-only token is ignored"
		);
	}
	if let Some(backup) = &settings.backup {
		crate::log_info!(
			"automated backups every {:?} to {} (keeping {})",
			backup.interval,
			backup.dir.display(),
			backup.keep
		);
	}

	// Background maintenance: periodic WAL checkpointing and automated
	// backups. Both are spawn_blocking tasks around `Db` methods and share
	// the pool safely with the request handlers.
	if settings.wal_checkpoint_secs > 0 {
		tokio::spawn(wal_checkpoint_loop(
			db_arc.clone(),
			settings.wal_checkpoint_secs,
		));
	}
	if let Some(backup) = settings.backup.clone() {
		tokio::spawn(backup_loop(db_arc.clone(), backup));
	}

	let app = app(state);
	// A bind failure (most often "Address already in use" — another service
	// holds the port) means the server cannot run at all. Log it as fatal
	// and terminate the process rather than returning an error: there is
	// nothing left to serve, so continuing would just sit in a broken loop.
	let listener =
		match tokio::net::TcpListener::bind((settings.host.as_str(), settings.port)).await {
			Ok(listener) => listener,
			Err(err) => {
				crate::log_fatal!(
					"cannot bind to {}:{}: {err} — is another service already using that port?",
					settings.host,
					settings.port
				);
				std::process::exit(1);
			}
		};
	crate::log_debug!("router assembled, listener bound");
	crate::log_info!(
		"waypointd listening on http://{}:{}",
		settings.host,
		settings.port
	);
	// `into_make_service_with_connect_info` is what lets handlers read
	// the client's address via `ConnectInfo` — needed for per-request
	// logging and any future rate limiting.
	axum::serve(
		listener,
		app.into_make_service_with_connect_info::<SocketAddr>(),
	)
	.with_graceful_shutdown(shutdown_signal())
	.await?;

	// Graceful shutdown stops the listener and lets in-flight requests
	// finish, then we land here. Before the pool drops (which closes every
	// SQLite connection) run one final TRUNCATE checkpoint so the WAL is
	// merged into the main database file and emptied; when the last
	// connection closes SQLite removes the `-wal`/`-shm` sidecars.
	db_arc.checkpoint();
	crate::log_info!("waypointd server stopped");

	Ok(())
}

/// Periodic `PASSIVE` WAL checkpoint so the WAL file doesn't grow without
/// bound between restarts. Best effort by design — a checkpoint that can't
/// run this tick just waits for the next.
async fn wal_checkpoint_loop(db: Arc<database::Db>, secs: u64) {
	let mut ticker = tokio::time::interval(Duration::from_secs(secs));
	ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
	loop {
		ticker.tick().await;
		let db = db.clone();
		let _ = tokio::task::spawn_blocking(move || db.wal_checkpoint_passive()).await;
	}
}

/// Automated-backup loop: `VACUUM INTO` snapshot every `cfg.interval`, then
/// prune the oldest beyond `cfg.keep`. The first backup waits one full
/// interval so a fresh start isn't slowed by a snapshot it didn't ask for.
async fn backup_loop(db: Arc<database::Db>, cfg: BackupConfig) {
	let start = tokio::time::Instant::now() + cfg.interval;
	let mut ticker = tokio::time::interval_at(start, cfg.interval);
	ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
	loop {
		ticker.tick().await;
		let dest = cfg
			.dir
			.join(database::backup_filename(&chrono::Local::now()));
		let dest_display = dest.display().to_string();
		let db = db.clone();
		let backup_dest = dest;
		match tokio::task::spawn_blocking(move || db.backup(&backup_dest)).await {
			Ok(Ok(())) => {
				crate::log_info!("backup written to {dest_display}");
				let dir = cfg.dir.clone();
				let keep = cfg.keep;
				let _ =
					tokio::task::spawn_blocking(move || database::prune_backups(&dir, keep)).await;
			}
			Ok(Err(err)) => crate::log_warn!("backup to {dest_display} failed: {err:#}"),
			Err(err) => crate::log_warn!("backup task failed: {err}"),
		}
	}
}

/// Waits for a shutdown request: SIGINT (Ctrl-C) or SIGTERM (a supervisor,
/// `kill`, ...). Both are the polite "stop now" signals; the process exits
/// through the normal drop path afterwards, so SQLite gets to close its
/// connections and delete the WAL sidecar files instead of the OS tearing
/// the process down mid-flight.
async fn shutdown_signal() {
	#[cfg(unix)]
	let terminate = async {
		tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
			.expect("failed to install SIGTERM handler")
			.recv()
			.await;
	};
	#[cfg(not(unix))]
	let terminate = std::future::pending::<()>();

	tokio::select! {
		_ = tokio::signal::ctrl_c() => {
			crate::log_info!("received SIGINT — shutting down");
		}
		_ = terminate => {
			crate::log_info!("received SIGTERM — shutting down");
		}
	}
}

/// Logs every request as it completes, with its duration, and separately
/// flags failures the handlers didn't already log. Also feeds the RED
/// metrics (counter, histogram, in-flight gauge) on every request.
///
/// Handlers return `Result<_, AppError>`, and `AppError::into_response`
/// logs its own rejection with the error code + message. This middleware
/// records an `info` line for every request (method, path, status,
/// elapsed) — the request-level access log, visible at the default `info`
/// level — and picks up the *other* ways a request goes unfulfilled —
/// axum extractor rejections (malformed JSON body, bad query string,
/// unparseable path segment), missing static assets, and anything else
/// that returns a 4xx/5xx without passing through `AppError`. The
/// `x-waypoint-error` header marks responses that were already logged, so
/// nothing is reported twice. 4xx -> warn, 5xx -> error.
async fn log_request(State(state): State<AppState>, req: Request, next: Next) -> Response {
	let started = Instant::now();
	let method = req.method().clone();
	let path = req.uri().path().to_owned();
	// One request context per request, with the request id as a field —
	// every `log_*!` line produced while handling this request carries it,
	// so a request's log lines can be grepped out together even when they
	// interleave.
	let ctx = crate::logging::RequestCtx::new(method.as_str(), &path);
	let response = crate::logging::with_request(ctx, next.run(req)).await;
	let status = response.status();
	let elapsed = started.elapsed();
	state
		.metrics
		.observe(method.as_str(), &path, status.as_u16(), elapsed);
	crate::log_info!("{method} {path}: {status} in {elapsed:?}");
	if (status.is_client_error() || status.is_server_error())
		&& !response.headers().contains_key(X_WAYPOINT_ERROR)
	{
		if status.is_server_error() {
			crate::log_error!("{method} {path}: failed with {status} after {elapsed:?}");
		} else {
			crate::log_warn!("{method} {path}: rejected with {status} after {elapsed:?}");
		}
	}
	response
}

/// Per-request deadline for the `/api` router. A request that exceeds
/// `state.request_timeout` (including time spent queued on the concurrency
/// semaphore) is answered 504 `request_timeout` instead of hanging.
