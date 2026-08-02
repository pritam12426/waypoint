/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Binary entry point — the thinnest possible shell over the library crate.
//!
//! `waypointd` is server-only: there is no CLI surface, so this file only
//! has to:
//!
//! 1. read configuration from `WAYPOINTD_*` environment variables
//!    (`src/config.rs`);
//! 2. initialize the structured logger once (default `info` so startup and
//!    per-request failures are visible);
//! 3. hand off to `http::run` (async, needs the tokio runtime).
//!
//! Both debug and release builds embed the frontend the same way.

use waypointd::config;
use waypointd::http::{BackupConfig, Settings};
use waypointd::logging::{LogFormat, LogLevel};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// Logging defaults: `info` so startup/shutdown and per-request
	// failures are visible. Every setting can be overridden with the
	// `WAYPOINTD_LOG_*` env vars — see `src/logging/`.
	waypointd::logging::log_init(None, LogLevel::Info, LogFormat::Pretty);

	let settings = Settings {
		db_path: config::db_file(),
		host: config::host(),
		port: config::port(),
		api_token: config::api_token(),
		read_token: config::read_token(),
		cookie_secure: config::cookie_secure(),
		wal_checkpoint_secs: config::wal_checkpoint_secs(),
		backup: config::backup_dir().map(|dir| BackupConfig {
			interval: std::time::Duration::from_secs(config::backup_interval_secs()),
			keep: config::backup_keep(),
			dir,
		}),
		request_timeout: std::time::Duration::from_secs(config::request_timeout_secs()),
		max_concurrency: config::max_concurrency(),
	};

	waypointd::log_debug!(
		"config: db={} host={} port={} api_token={} read_token={}",
		settings.db_path.display(),
		settings.host,
		settings.port,
		if settings.api_token.is_some() {
			"enabled"
		} else {
			"disabled"
		},
		if settings.read_token.is_some() {
			"enabled"
		} else {
			"disabled"
		}
	);

	let result = waypointd::http::run(settings).await;
	// A crashed server is the one place `fatal` belongs: the process is
	// about to exit with a non-zero status.
	if let Err(err) = &result {
		waypointd::log_fatal!("{err:#}");
	}
	result
}
