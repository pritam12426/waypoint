/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Server configuration — environment-variable driven, with the defaults
//! defined once here.
//!
//! `waypointd` is server-only: every setting comes from the environment
//! (`WAYPOINTD_*`). The only CLI surface is three informational flags in
//! `main.rs` (`--help`/`--version`/`--config`) that print and exit — they
//! read this module but never configure anything. Keeping the defaults here
//! (rather than scattered across `main.rs` and `http`) means one value
//! can't drift between layers.
//!
//! # Environment variables
//!
//! * `WAYPOINTD_DB_FILE`   — SQLite database path (default `waypoint.sqlite`)
//! * `WAYPOINTD_DB_CACHE_SIZE`— SQLite page-cache size per connection, in
//!   KiB (default `32768` = 32 MiB; `0` lets SQLite pick)
//! * `WAYPOINTD_DB_MMAP_SIZE`— SQLite read-only mmap ceiling in bytes
//!   (default `268435456` = 256 MiB; `0` disables mmap)
//! * `WAYPOINTD_SERVE_HOST`— bind host (default `localhost`)
//! * `WAYPOINTD_SERVE_PORT`— bind port (default `8080`)
//! * `WAYPOINTD_SERVE_TOKEN`— optional full-access token for `/api/*` + docs
//! * `WAYPOINTD_READ_TOKEN`— optional read-only token (GET/HEAD only)
//! * `WAYPOINTD_COOKIE_SECURE`— `true` to set `Secure` on the session cookie
//!   (default `false` — the common self-hosted shape is plain HTTP)
//! * `WAYPOINTD_WAL_CHECKPOINT_SECS`— seconds between periodic WAL
//!   checkpoints (default `60`; `0` disables the background task)
//! * `WAYPOINTD_BACKUP_DIR` — optional directory for automated backups;
//!   when set, `VACUUM INTO` snapshots are written here on a timer and via
//!   `POST /api/admin/backup`
//! * `WAYPOINTD_BACKUP_INTERVAL_SECS` — seconds between automated backups
//!   (default `86400` = daily)
//! * `WAYPOINTD_BACKUP_KEEP` — how many backups to retain (default `7`)
//! * `WAYPOINTD_REQUEST_TIMEOUT_SECS` — per-request timeout (default `30`)
//! * `WAYPOINTD_MAX_CONCURRENCY` — concurrent API requests before 503
//!   (default `64`)
//! * `WAYPOINTD_LOG_LEVEL` / `WAYPOINTD_LOG_FORMAT` / `WAYPOINTD_LOG_FILE`—
//!   see `src/logging/`
//! * `WAYPOINTD_CACHE_DIR` — fetched-media cache dir (see `src/core/cache.rs`)

use std::path::PathBuf;

/// Default SQLite database file.
pub const DEFAULT_DB_FILE: &str = "waypoint.sqlite";

/// Default per-connection page cache, in KiB (the `cache_size` pragma's
/// negative/KiB form). 32 MiB is the fixed value the pool always used.
pub const DEFAULT_DB_CACHE_SIZE_KIB: i64 = 32 * 1024;

/// Default read-only mmap ceiling, in bytes (the `mmap_size` pragma). This
/// is *virtual* address space, not committed RAM — pages are faulted in on
/// demand and evictable under pressure, so reads avoid page-cache syscalls.
pub const DEFAULT_DB_MMAP_SIZE: i64 = 256 * 1024 * 1024;

/// Default host to bind (`localhost` — deliberately not `0.0.0.0`, so the
/// server is only reachable on this machine unless the user asks for more).
pub const DEFAULT_HOST: &str = "localhost";

/// Default port to listen on.
pub const DEFAULT_PORT: u16 = 8080;

/// Default page size for `list` (HTTP default when no `limit`).
pub const DEFAULT_LIST_LIMIT: i64 = 50;

/// Default page size for `search`.
pub const DEFAULT_SEARCH_LIMIT: i64 = 20;

/// Default seconds between periodic WAL checkpoints (0 disables the task).
pub const DEFAULT_WAL_CHECKPOINT_SECS: u64 = 60;

/// Default seconds between automated backups (once a day).
pub const DEFAULT_BACKUP_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// How many automated backups to retain before pruning the oldest.
pub const DEFAULT_BACKUP_KEEP: usize = 7;

/// Default request timeout — a single request gets this long (queue wait +
/// handler) before the server answers 504.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Default cap on concurrently-executing API requests. Saturation answers
/// 503 instead of piling unbounded backlog onto the SQLite pool.
pub const DEFAULT_MAX_CONCURRENCY: usize = 64;

/// Env-var helper: reads a `WAYPOINTD_*` unsigned-integer setting, falling
/// back to `default` on unset/empty/non-numeric values.
fn env_u64(name: &str, default: u64) -> u64 {
	std::env::var(name)
		.ok()
		.and_then(|v| v.parse().ok())
		.unwrap_or(default)
}

/// Reads the SQLite database path from `WAYPOINTD_DB_FILE`.
pub fn db_file() -> PathBuf {
	std::env::var_os("WAYPOINTD_DB_FILE")
		.map(PathBuf::from)
		.filter(|p| !p.as_os_str().is_empty())
		.unwrap_or_else(|| PathBuf::from(DEFAULT_DB_FILE))
}

/// Per-connection page cache in KiB, from `WAYPOINTD_DB_CACHE_SIZE`
/// (default 32768 = 32 MiB). `0` is passed through — SQLite then uses its
/// own default (~2 MiB).
pub fn db_cache_size_kib() -> i64 {
	env_u64("WAYPOINTD_DB_CACHE_SIZE", DEFAULT_DB_CACHE_SIZE_KIB as u64) as i64
}

/// Read-only mmap ceiling in bytes, from `WAYPOINTD_DB_MMAP_SIZE`
/// (default 256 MiB). `0` disables mmap entirely.
pub fn db_mmap_size() -> i64 {
	env_u64("WAYPOINTD_DB_MMAP_SIZE", DEFAULT_DB_MMAP_SIZE as u64) as i64
}

/// Reads the bind host from `WAYPOINTD_SERVE_HOST`.
pub fn host() -> String {
	std::env::var("WAYPOINTD_SERVE_HOST")
		.ok()
		.filter(|s| !s.is_empty())
		.unwrap_or_else(|| DEFAULT_HOST.to_string())
}

/// Reads the bind port from `WAYPOINTD_SERVE_PORT`.
pub fn port() -> u16 {
	std::env::var("WAYPOINTD_SERVE_PORT")
		.ok()
		.and_then(|p| p.parse().ok())
		.filter(|p| *p != 0)
		.unwrap_or(DEFAULT_PORT)
}

/// Reads the optional API bearer token from `WAYPOINTD_SERVE_TOKEN`.
/// An empty value means "auth disabled".
pub fn api_token() -> Option<String> {
	std::env::var("WAYPOINTD_SERVE_TOKEN")
		.ok()
		.filter(|s| !s.is_empty())
}

/// Reads the optional read-only token from `WAYPOINTD_READ_TOKEN`.
/// Unlike `WAYPOINTD_SERVE_TOKEN` this one grants GET/HEAD access only;
/// every mutating request is rejected with 403. An empty value means
/// "no read-only token configured".
pub fn read_token() -> Option<String> {
	std::env::var("WAYPOINTD_READ_TOKEN")
		.ok()
		.filter(|s| !s.is_empty())
}

/// Whether the session cookie carries the `Secure` attribute. Defaults to
/// `false` (plain HTTP on the local network is the common self-hosted
/// shape); set `true` when serving over TLS via a reverse proxy.
pub fn cookie_secure() -> bool {
	std::env::var("WAYPOINTD_COOKIE_SECURE")
		.ok()
		.map(|v| v.eq_ignore_ascii_case("true"))
		.unwrap_or(false)
}

/// Seconds between periodic WAL checkpoints (default 60).
pub fn wal_checkpoint_secs() -> u64 {
	env_u64("WAYPOINTD_WAL_CHECKPOINT_SECS", DEFAULT_WAL_CHECKPOINT_SECS)
}

/// Optional backup directory; `None` disables automated backups.
pub fn backup_dir() -> Option<PathBuf> {
	std::env::var_os("WAYPOINTD_BACKUP_DIR")
		.map(PathBuf::from)
		.filter(|p| !p.as_os_str().is_empty())
}

/// Seconds between automated backups (default daily).
pub fn backup_interval_secs() -> u64 {
	env_u64(
		"WAYPOINTD_BACKUP_INTERVAL_SECS",
		DEFAULT_BACKUP_INTERVAL_SECS,
	)
}

/// How many automated backups to retain before pruning the oldest.
pub fn backup_keep() -> usize {
	let v = env_u64("WAYPOINTD_BACKUP_KEEP", DEFAULT_BACKUP_KEEP as u64);
	// A keep of 0 would delete the backup we just made — clamp to 1.
	v.max(1) as usize
}

/// Per-request timeout in seconds (default 30).
pub fn request_timeout_secs() -> u64 {
	env_u64(
		"WAYPOINTD_REQUEST_TIMEOUT_SECS",
		DEFAULT_REQUEST_TIMEOUT_SECS,
	)
}

/// Cap on concurrently-executing API requests (default 64).
pub fn max_concurrency() -> usize {
	let v = env_u64("WAYPOINTD_MAX_CONCURRENCY", DEFAULT_MAX_CONCURRENCY as u64);
	// 0 would reject every request outright — clamp to 1.
	v.max(1) as usize
}
