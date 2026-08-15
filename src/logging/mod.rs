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
//! Timestamps always print; source locations are gated on
//! `debug_assertions`, so they only appear in debug builds (compile out in
//! release).
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
	fn from_env_str(s: &str) -> Option<Self> {
		match s.to_ascii_lowercase().as_str() {
			"human-readable" | "pretty" | "text" | "human" => Some(LogFormat::Pretty),
			"json" => Some(LogFormat::Json),
			_ => None,
		}
	}
}

// ── Output stream: either stderr or an opened file ─────────────────────────
enum Output {
	Stderr,
	File(std::fs::File),
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

/// Allocates a new correlation id for one inbound HTTP request. The HTTP
/// middleware builds a [`RequestCtx`] from one of these per request, so a
/// single request's log lines can be grepped out together even when
/// requests interleave.
pub fn next_request_id() -> u64 {
	NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// A per-request correlation context, attached to every log line emitted
/// while handling one HTTP request.
#[derive(Debug, Clone)]
pub struct RequestCtx {
	pub req_id: u64,
	pub method: String,
	pub path: String,
}

impl RequestCtx {
	pub fn new(method: &str, path: &str) -> Self {
		Self {
			req_id: next_request_id(),
			method: method.to_owned(),
			path: path.to_owned(),
		}
	}
}

tokio::task_local! {
	/// The request context of the request currently being handled on this
	/// task. Read with `try_with` (never `with`) everywhere: logging from a
	/// `spawn_blocking` thread, or from outside any task, must degrade to
	/// "no context" instead of panicking.
	static CURRENT_REQUEST: RequestCtx;
}

/// Runs `f` with `ctx` as the current request's correlation context. Every
/// `log_*!` line emitted by `f` — including through `.await` points, since
/// tokio restores task-locals on every poll — carries the request's
/// `req_id`/`method`/`path`.
pub async fn with_request<F>(ctx: RequestCtx, f: F) -> F::Output
where
	F: std::future::Future,
{
	CURRENT_REQUEST.scope(ctx, f).await
}

// ── Public API ───────────────────────────────────────────────────────────

/// Initialise the logger. Thread-safe; may be called multiple times.
///
/// `WAYPOINTD_LOG_LEVEL` in the environment (values: off/fatal/error/warn/
/// info/debug/trace) takes precedence over `level` when set and valid, as
/// do `WAYPOINTD_LOG_FORMAT` (`human-readable`/`json`) over `format` and
/// `WAYPOINTD_LOG_FILE` over `file_path` — this lets an operator bump
/// verbosity for one run without changing how the process is invoked.
pub fn log_init(file_path: Option<&str>, level: LogLevel, format: LogFormat) {
	let level = std::env::var("WAYPOINTD_LOG_LEVEL")
		.ok()
		.and_then(|v| LogLevel::from_env_str(&v))
		.unwrap_or(level);
	let format = std::env::var("WAYPOINTD_LOG_FORMAT")
		.ok()
		.and_then(|v| LogFormat::from_env_str(&v))
		.unwrap_or(format);
	let file_path = std::env::var("WAYPOINTD_LOG_FILE")
		.ok()
		.filter(|p| !p.is_empty())
		.or_else(|| file_path.map(str::to_owned));

	let (stream, use_color) = match file_path {
		None => (Output::Stderr, io::stderr().is_terminal()),
		Some(path) => match std::fs::OpenOptions::new()
			.create(true)
			.append(true)
			.open(&path)
		{
			Ok(f) => (Output::File(f), false),
			Err(err) => {
				eprintln!(
					"[logging] warning: could not open log file '{path}' ({err}); falling back to stderr"
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
	// Only the level name is colored — the brackets stay plain. Each name is
	// padded to 5 characters so the `]` aligns across levels.
	let (name, color) = match level {
		LogLevel::Fatal => ("FATAL", BOLD_RED),
		LogLevel::Error => ("ERROR", BOLD_RED),
		LogLevel::Warn => ("WARN ", BOLD_YELLOW),
		LogLevel::Info => ("INFO ", BOLD_GREEN),
		LogLevel::Debug => ("DEBUG", BOLD_CYAN),
		LogLevel::Trace => ("TRACE", BOLD_MAGENTA),
		LogLevel::Off => ("UNKWN", BOLD_BLUE),
	};
	write!(out, "[{color}{name}{RESET}] ")
}

fn timestamp_str() -> String {
	Local::now().format("%d-%b-%Y %H:%M:%S%.6f").to_string()
}

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

/// The `http{method=.. path=.. req_id=..}: ` prefix that marks a line as
/// belonging to one in-flight request, mirroring what the old tracing-based
/// span output looked like.
fn write_request_ctx(out: &mut dyn Write, ctx: &RequestCtx) -> io::Result<()> {
	write!(
		out,
		"http{{method={} path={} req_id={}}}: ",
		ctx.method, ctx.path, ctx.req_id
	)
}

/// Source-location info captured at the call site. Only ever constructed
/// in debug builds — see `__loc!()`.
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
			"{}[logging] error: log_init() not called — dropping message{}",
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

	// The correlation context of the request currently being handled on
	// this task, if any. `try_with` so logging from `spawn_blocking` threads
	// (which do not inherit task-locals) or outside any task stays silent
	// about it instead of panicking.
	let request = CURRENT_REQUEST.try_with(|ctx| ctx.clone()).ok();

	let use_color = state.use_color;
	let format = state.format;
	let stream = state.stream.as_mut().unwrap();

	match format {
		LogFormat::Json => {
			let mut obj = serde_json::json!({
				"level": level_label_json(level),
				"msg": msg,
			});
			obj["ts"] = serde_json::json!(timestamp_str());
			if let Some(l) = &loc {
				obj["file"] = serde_json::json!(l.file);
				obj["line"] = serde_json::json!(l.line);
				obj["func"] = serde_json::json!(l.func);
			}
			if let Some(ctx) = &request {
				obj["req_id"] = serde_json::json!(ctx.req_id);
				obj["method"] = serde_json::json!(ctx.method);
				obj["path"] = serde_json::json!(ctx.path);
			}
			let _ = write!(stream, "{}", obj);
			if new_line {
				let _ = writeln!(stream);
			}
		}
		LogFormat::Pretty => {
			let _ = write_time_stamp(stream, use_color);

			let _ = if use_color {
				write_color_label(stream, level)
			} else {
				write!(stream, "{}", level_label_plain(level))
			};

			if let Some(ctx) = &request {
				let _ = write_request_ctx(stream, ctx);
			}

			#[cfg(debug_assertions)]
			if let Some(l) = &loc {
				let (pre, post) = if use_color {
					(color::DIM, color::RESET)
				} else {
					("", "")
				};
				let _ = write!(stream, "{}[{}:{}:{}]{} ", pre, l.file, l.line, l.func, post);
			}
			#[cfg(not(debug_assertions))]
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
