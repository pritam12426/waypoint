/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! waypoint's structured logger.
//!
//! Kept as a hand-rolled logger rather than switching to `tracing`: the
//! server has exactly one sink (stderr or a single file), no async
//! task-local spans, and no subscriber ecosystem to plug into — `tracing`'s
//! value is in exactly those things. What this gives us instead:
//!
//!   * a machine-parseable **JSON log format**, toggled with `--log-format
//!     json`, so logs can be piped into `jq` / a log aggregator without
//!     regex-scraping the pretty format;
//!   * an **environment-variable override** (`WAYPOINT_LOG_LEVEL`), so the
//!     verbosity can be bumped for a single run without touching the CLI
//!     invocation — handy when waypoint is launched by another program
//!     (systemd, a supervisor, a container entrypoint) that doesn't pass
//!     `--log-level` through;
//!   * a per-request **correlation id**, so every log line produced while
//!     handling one HTTP request can be grepped out together, which
//!     matters once requests start interleaving under concurrent load.
//!
//! Compile-time feature gates (`show_time_stamp`, `show_source_location`)
//! compile out entirely when disabled, so there's no runtime cost for a
//! feature you're not using. Both are default-on: timestamps always show,
//! and source locations only in debug builds (the code paths are further
//! gated on `debug_assertions`).

use clap::ValueEnum;
use std::fs::{File, OpenOptions};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
#[clap(rename_all = "lowercase")]
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
	/// Parses a level from a string (used for the `WAYPOINT_LOG_LEVEL`
	/// env override, which arrives as a plain string, not through clap).
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
#[clap(rename_all = "lowercase")]
pub enum LogFormat {
	/// Human-readable, colourized when writing to a TTY.
	#[default]
	Pretty,
	/// One JSON object per line: `{"ts":..,"level":..,"msg":..}`.
	Json,
}

// ── Output stream: either stderr or an opened file ─────────────────────────
enum Output {
	Stderr,
	File(File),
}

impl Write for Output {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		match self {
			Output::Stderr => io::stderr().write(buf),
			Output::File(f) => f.write(buf),
		}
	}
	fn flush(&mut self) -> io::Result<()> {
		match self {
			Output::Stderr => io::stderr().flush(),
			Output::File(f) => f.flush(),
		}
	}
}

struct LoggerState {
	stream: Option<Output>,
	level: LogLevel,
	use_color: bool,
	format: LogFormat,
}

static LOGGER: OnceLock<Mutex<LoggerState>> = OnceLock::new();

fn logger() -> &'static Mutex<LoggerState> {
	LOGGER.get_or_init(|| {
		Mutex::new(LoggerState {
			stream: None,
			level: LogLevel::Warn,
			use_color: false,
			format: LogFormat::Pretty,
		})
	})
}

// ── Request correlation IDs ─────────────────────────────────────────────────

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Allocates a new correlation id for one inbound HTTP request. A handler
/// (or middleware) calls this once per request and threads the result
/// through its log lines (`req_id=<n>`), so a single request's log lines
/// can be grepped out together even when requests interleave.
pub fn next_request_id() -> u64 {
	NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

// ── Public API ───────────────────────────────────────────────────────────

/// Initialise the logger. Thread-safe; may be called multiple times.
///
/// `WAYPOINT_LOG_LEVEL` in the environment (values: off/fatal/error/warn/
/// info/debug/trace) takes precedence over `level` when set and valid —
/// this lets an operator bump verbosity for one run without changing how
/// the process is invoked.
pub fn log_init(file_path: Option<&str>, level: LogLevel, format: LogFormat) {
	let level = std::env::var("WAYPOINT_LOG_LEVEL")
		.ok()
		.and_then(|v| LogLevel::from_env_str(&v))
		.unwrap_or(level);

	let (stream, use_color) = match file_path {
		None => (Output::Stderr, io::stderr().is_terminal()),
		Some(path) => match OpenOptions::new().create(true).append(true).open(path) {
			Ok(f) => (Output::File(f), false),
			Err(_) => {
				eprintln!(
					"[LOG] warning: could not open log file '{}', falling back to stderr",
					path
				);
				(Output::Stderr, io::stderr().is_terminal())
			}
		},
	};

	let mut state = logger().lock().unwrap();
	state.stream = Some(stream);
	state.use_color = use_color;
	state.level = level;
	state.format = format;
}

pub fn log_set_level(level: LogLevel) {
	logger().lock().unwrap().level = level;
}

pub fn log_get_level() -> LogLevel {
	logger().lock().unwrap().level
}

pub fn log_use_color() -> bool {
	logger().lock().unwrap().use_color
}

fn level_label_plain(level: LogLevel) -> &'static str {
	match level {
		LogLevel::Fatal => "[FATAL] ",
		LogLevel::Error => "[ERROR] ",
		LogLevel::Warn => "[WARN ] ",
		LogLevel::Info => "[INFO ] ",
		LogLevel::Debug => "[DEBUG] ",
		LogLevel::Trace => "[TRACE] ",
		LogLevel::Off => "[UNKWN] ",
	}
}

fn level_label_json(level: LogLevel) -> &'static str {
	match level {
		LogLevel::Fatal => "fatal",
		LogLevel::Error => "error",
		LogLevel::Warn => "warn",
		LogLevel::Info => "info",
		LogLevel::Debug => "debug",
		LogLevel::Trace => "trace",
		LogLevel::Off => "unknown",
	}
}

fn write_color_label(out: &mut dyn Write, level: LogLevel) -> io::Result<()> {
	use color::*;
	match level {
		LogLevel::Fatal => write!(out, "\u{1F480} [{BOLD_BLUE}FATAL{RESET}] "),
		LogLevel::Error => write!(out, "\u{1F6A8} [{BOLD_RED}ERROR{RESET}] "),
		LogLevel::Warn => write!(out, "\u{26A0}\u{FE0F}  [{BOLD_YELLOW}WARN {RESET}] "),
		LogLevel::Info => write!(out, "\u{2139}\u{FE0F}  [{BOLD_GREEN}INFO {RESET}] "),
		LogLevel::Debug => write!(out, "\u{1F6E0}\u{FE0F}  [{BOLD_CYAN}DEBUG{RESET}] "),
		LogLevel::Trace => write!(out, "\u{1F52C} [{BOLD_MAGENTA}TRACE{RESET}] "),
		LogLevel::Off => write!(out, "[{BOLD_BLUE}UNKWN{RESET}] "),
	}
}

#[cfg(feature = "show_time_stamp")]
fn timestamp_str() -> String {
	Local::now().format("%d-%b-%Y %H:%M:%S%.6f").to_string()
}

#[cfg(feature = "show_time_stamp")]
fn write_time_stamp(out: &mut dyn Write, use_color: bool) -> io::Result<()> {
	if use_color {
		write!(out, "{}", color::DIM)?;
	}
	write!(out, "[{}] ", timestamp_str())?;
	if use_color {
		write!(out, "{}", color::RESET)?;
	}
	Ok(())
}

/// Source-location info captured at the call site. Only ever constructed
/// when the `show_source_location` feature is enabled — see `__loc!()`.
#[doc(hidden)]
pub struct SourceLoc {
	pub file: &'static str,
	pub line: u32,
	pub func: &'static str,
}

/// Core logging function: formats and writes a log message. Called by the
/// `log_*!` macros — prefer those over calling this directly.
#[doc(hidden)]
pub fn log_record(level: LogLevel, loc: Option<SourceLoc>, new_line: bool, msg: &str) {
	let mut state = logger().lock().unwrap();

	if state.stream.is_none() {
		let _ = write!(
			io::stderr(),
			"{}[LOG] error: log_init() not called — dropping message{}",
			color::BOLD_RED,
			color::RESET
		);
		if new_line {
			let _ = writeln!(io::stderr());
		}
		return;
	}

	if (level as i32) > (state.level as i32) {
		return;
	}

	let use_color = state.use_color;
	let format = state.format;
	let stream = state.stream.as_mut().unwrap();

	match format {
		LogFormat::Json => {
			let mut obj = serde_json::json!({
				"level": level_label_json(level),
				"msg": msg,
			});
			#[cfg(feature = "show_time_stamp")]
			{
				obj["ts"] = serde_json::json!(timestamp_str());
			}
			if let Some(l) = &loc {
				obj["file"] = serde_json::json!(l.file);
				obj["line"] = serde_json::json!(l.line);
				obj["func"] = serde_json::json!(l.func);
			}
			let _ = write!(stream, "{}", obj);
			if new_line {
				let _ = writeln!(stream);
			}
		}
		LogFormat::Pretty => {
			#[cfg(feature = "show_time_stamp")]
			let _ = write_time_stamp(stream, use_color);

			let _ = if use_color {
				write_color_label(stream, level)
			} else {
				write!(stream, "{}", level_label_plain(level))
			};

			#[cfg(all(feature = "show_source_location", debug_assertions))]
			if let Some(l) = &loc {
				let (pre, post) = if use_color {
					(color::DIM, color::RESET)
				} else {
					("", "")
				};
				let _ = write!(stream, "{}[{}:{}:{}]{} ", pre, l.file, l.line, l.func, post);
			}
			#[cfg(not(all(feature = "show_source_location", debug_assertions)))]
			let _ = &loc;

			let _ = write!(stream, "{}", msg);
			if new_line {
				let _ = writeln!(stream);
			}
		}
	}
	let _ = stream.flush();
}

/// Truncates a `Display`-able value for safe inclusion in a log line — a
/// request body or bookmark note can be arbitrarily large, and logging it
/// in full would both bloat the logs and make single-line JSON
/// grep-ability worse. Full detail is still available at `debug`/`trace`
/// level via callers that choose to bypass this.
pub fn truncate_for_log(s: &str, max_chars: usize) -> String {
	if s.chars().count() <= max_chars {
		s.to_string()
	} else {
		let head: String = s.chars().take(max_chars).collect();
		format!("{head}… ({} chars total)", s.chars().count())
	}
}
