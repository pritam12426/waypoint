/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Binary entry point — the thinnest possible shell over the library crate.
//!
//! # Why this file is so small
//!
//! Every piece of real logic lives in `waypoint` (the library crate): CLI
//! parsing and dispatch in `cmd`, the server in `http`, the logger in
//! `logging`. This file only has to:
//!
//! 1. parse the CLI (`Cli::parse()`);
//! 2. pick a default log level (`warn` for one-shot commands, `info` while
//!    serving — serving is long-lived, so verbose logs are useful there);
//! 3. initialize logging once;
//! 4. hand off to `http::run` (async, needs the tokio runtime) or
//!    `cmd::run_command` (sync, opens its own connection).
//!
//! The two `#[cfg]` variants of the `Serve` arm exist because `--static-dir`
//! (a live-frontend override) only exists in **debug** builds. A release
//! binary always serves the embedded `frontend/`, so it passes `None`. This
//! is the one place the debug/release difference is visible.

use clap::Parser;
use waypoint::cmd::{Cli, Command};
use waypoint::logging::LogLevel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let cli = Cli::parse();

	// Logging default is context-dependent: brief one-shot commands are
	// noisy at `info`, but a long-running server benefits from it. The
	// user can always override with `--log-level`.
	let log_level = cli.log_level.unwrap_or(match &cli.command {
		Command::Serve { .. } => LogLevel::Info,
		_ => LogLevel::Warn,
	});

	waypoint::logging::log_init(cli.log_file.as_deref(), log_level, cli.log_format);
	waypoint::log_debug!("parsed CLI: {cli:?}");

	match cli.command {
		// Debug build: `--static-dir` is available, so serve from disk.
		#[cfg(debug_assertions)]
		Command::Serve {
			host,
			port,
			static_dir,
			api_token,
		} => waypoint::http::run(cli.database, &host, port, static_dir, api_token).await,
		// Release build: no `--static-dir` flag, always embedded assets.
		#[cfg(not(debug_assertions))]
		Command::Serve {
			host,
			port,
			api_token,
			..
		} => waypoint::http::run(cli.database, &host, port, None, api_token).await,
		// Everything else (bookmarks/tags/categories/trash/stats/check)
		// is a synchronous one-shot command.
		command => waypoint::cmd::run_command(&cli.database, command),
	}
}
