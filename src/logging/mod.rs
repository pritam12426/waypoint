/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! waypointd's structured logger.
//!
//! Kept as a hand-rolled logger rather than switching to `tracing`: the
//! server has exactly one sink (stderr or a single file), no async
//! task-local spans, and no subscriber ecosystem to plug into — `tracing`'s
//! value is in exactly those things. What this gives us instead:
//!
//!   * a machine-parseable **JSON log format**, toggled with the
//!     `WAYPOINTD_LOG_FORMAT` env var, so logs can be piped into `jq` / a
//!     log aggregator without regex-scraping the pretty format;
//!   * an **environment-variable override** (`WAYPOINTD_LOG_LEVEL`), so the
//!     verbosity can be bumped for a single run without touching how the
//!     process is invoked — handy when waypointd is launched by another
//!     program (systemd, a supervisor, a container entrypoint);
//!   * a per-request **correlation context**, so every log line produced
//!     while handling one HTTP request carries its `req_id` (plus method
//!     and path), which matters once requests start interleaving under
//!     concurrent load.
//!
//! Compile-time feature gates (`show_time_stamp`, `show_source_location`)
//! compile out entirely when disabled, so there's no runtime cost for a
//! feature you're not using. Both are default-on: timestamps always show,
//! and source locations only in debug builds (the code paths are further
//! gated on `debug_assertions`).
//!
//! # Output shapes
//!
//! Human-readable lines look like:
//!
//! ```text
//! [12-Aug-2026 03:18:50.123456] [INFO ] http{method=POST path=/api/bookmarks req_id=1}: [handlers/mod.rs:451:create_bookmark] created bookmark #1
//! ```
//!
//! JSON lines are one compact object per line; the request context is
//! merged in (`req_id`/`method`/`path` sit alongside the message):
//!
//! ```text
//! {"level":"info","msg":"created bookmark #1","ts":"12-Aug-2026 03:18:50.123456","file":"...","line":451,"func":"...","req_id":1,"method":"POST","path":"/api/bookmarks"}
//! ```

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

#[cfg(feature = "show_time_stamp")]
use chrono::Local;

pub mod macros;

// ── ANSI colour codes ───────────────────────────────────────────────────────
mod color {
	pub const RESET: &str = "\x1b[0m";
	pub const BOLD_RED: &str = "\x1b[1;31m";
	pub const BOLD_GREEN: &str = "\x1b[1;32m";
	pub const BOLD_YELLOW: &str = "\x1b[1;33m";
	pub const BOLD_BLUE: &str = "\x1b[1;34m";
	pub const BOLD_MAGENTA: &str = "\x1b[1;35m";
	pub const BOLD_CYAN: &str = "\x1b[1;36m";
	#[allow(dead_code)]
	pub const DIM: &str = "\x1b[2m";
}

// ── Log levels (lower number = higher priority) ────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(i32)]
pub enum LogLevel {
	Off = 0,
	Fatal = 1,
	Error = 2,
	Warn = 3,
	Info = 4,
	Debug = 5,
	Trace = 6,
}

impl LogLevel {
	/// Parses a level from a string (used for the `WAYPOINTD_LOG_LEVEL`
	/// env override, which arrives as a plain string). Returns `None` for
	/// unknown values so callers can fall back to a default.
	pub fn from_env_str(s: &str) -> Option<Self> {
		match s.to_ascii_lowercase().as_str() {
			"off" => Some(LogLevel::Off),
			"fatal" => Some(LogLevel::Fatal),
			"error" => Some(LogLevel::Error),
			"warn" => Some(LogLevel::Warn),
			"info" => Some(LogLevel::Info),
			"debug" => Some(LogLevel::Debug),
			"trace" => Some(LogLevel::Trace),
			_ => None,
		}
	}
}

/// Output format for log lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
	/// Human-readable, colourized when writing to a TTY. The default; the
	/// `WAYPOINTD_LOG_FORMAT` env value is `human-readable`.
	#[default]
	Pretty,
	/// One JSON object per line: `{"level":..,"msg":..,"ts":..}`.
	Json,
}

impl LogFormat {
	/// Parses a format from a string (used for the `WAYPOINTD_LOG_FORMAT`
	/// env override).
