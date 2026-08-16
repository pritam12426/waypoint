/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Binary entry point — the thinnest possible shell over the library crate.
//!
//! `waypointd` is server-only: configuration comes from `WAYPOINTD_*`
//! environment variables, never from flags. The only CLI surface is three
//! informational flags that print and exit (see [`cli_action`]):
//!
//! * `-?` / `--help` — the program, the author, and every flag
//! * `-v` / `--version` — the version on stdout, then exit
//! * `-c` / `--config` — every configuration variable, whether it is set,
//!   its default, and its effective value
//!
//! Otherwise `main` only has to:
//!
//! 1. read configuration from `WAYPOINTD_*` environment variables
//!    (`src/config.rs`);
//! 2. initialize the structured logger once (default `info` so startup and
//!    per-request failures are visible);
//! 3. hand off to `http::run` (async, needs the tokio runtime).
//!
//! Both debug and release builds embed the frontend the same way.

use std::io::{self, IsTerminal};

use waypointd::config;
use waypointd::http::{BackupConfig, Settings};
use waypointd::logging::{LogFormat, LogLevel};

/// The three informational flags the binary accepts. An unknown flag exits
/// with status 2; no argument at all starts the server.
enum CliAction {
	Help,
	Version,
	Config,
}

/// Parses the command-line arguments, if any.
///
/// The informational flags take no arguments; an unknown flag or an extra
/// argument exits with status 2 (and a hint) instead of being silently
/// ignored — the server otherwise runs with whatever the environment says.
fn cli_action() -> Option<CliAction> {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let action = match args.first().map(String::as_str) {
		Some("-?" | "--help") => Some(CliAction::Help),
		Some("-v" | "--version") => Some(CliAction::Version),
		Some("-c" | "--config") => Some(CliAction::Config),
		Some(other) => {
			eprintln!("waypointd: unknown argument '{other}'");
			eprintln!("try 'waypointd --help' for usage");
			std::process::exit(2);
		}
		None => None,
	};
	if action.is_some() && args.len() > 1 {
		eprintln!("waypointd: '{}' takes no arguments", args[0]);
		eprintln!("try 'waypointd --help' for usage");
		std::process::exit(2);
	}
	action
}

fn cli_dispatch(action: CliAction) -> anyhow::Result<()> {
	match action {
		CliAction::Help => print_help(),
		CliAction::Version => println!("waypointd {}", env!("CARGO_PKG_VERSION")),
		CliAction::Config => print_config(),
	}
	Ok(())
}

fn print_help() {
	println!(
		"waypointd {} — {}",
		env!("CARGO_PKG_VERSION"),
		env!("CARGO_PKG_DESCRIPTION")
	);
	println!("Author: {}", env!("CARGO_PKG_AUTHORS"));
	println!();
	println!("Usage: waypointd [FLAG]");
	println!();
	println!("A self-hosted bookmark server with keyword shortcuts and full-text search.");
	println!("Configuration is entirely WAYPOINTD_* environment variables — there is no");
	println!("config file. 'waypointd --config' prints every setting, whether it is set,");
	println!("its default, and its effective value.");
	println!();
	println!("Flags:");
	println!("  -?, --help     Show this help and exit");
	println!("  -v, --version  Print the version and exit");
	println!("  -c, --config   Print the effective configuration and exit");
}

/// Prints every `WAYPOINTD_*` setting: effective value, default, and
/// whether the environment variable is set. Grouped by concern and
/// colourized only when stdout is a terminal, so piped output stays plain
/// and greppable. Colours mirror `logging`'s ANSI palette.
fn print_config() {
	let (cyan, green, dim, reset) = if io::stdout().is_terminal() {
		("\x1b[1;36m", "\x1b[1;32m", "\x1b[2m", "\x1b[0m")
	} else {
		("", "", "", "")
	};

	// An empty value means "unset" to every `config::*` accessor — mirror
	// that here so the dump and the server agree.
	let env_set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
	let raw = |name: &str, default: &str| {
		if env_set(name) {
			std::env::var(name).unwrap_or_default()
		} else {
			default.to_string()
		}
	};
	// The logging vars parse with fallback in `logging::log_init`, so a raw
	// value only takes effect when it parses; print it only in that case.
	let raw_if_valid = |name: &str, default: &str, ok: fn(&str) -> bool| {
		let v = raw(name, default);
		if env_set(name) && ok(&v) {
			v
		} else {
			default.to_string()
		}
	};
	// Tokens are never printed — just whether one is configured.
	let token = |name: &str| {
		if env_set(name) {
			"<set>".to_string()
		} else {
			"<unset>".to_string()
		}
	};
	// The longest variable name is 30 chars, so padding to 30 aligns every
	// value in one column.
	let line = |name: &str, value: String, default: &str| {
		let marker = if env_set(name) {
			format!("{green}set{dim}")
		} else {
			format!("{dim}unset{dim}")
		};
		println!(
			"  {cyan}{:<30}{reset} {green}{value}{reset}  {dim}(default: {default}, env: {marker}){reset}",
			name
		);
	};
	let section = |title: &str| println!("\n{cyan}{title}{reset}");

	println!(
		"{cyan}waypointd {} — effective configuration{reset}",
		env!("CARGO_PKG_VERSION")
	);
	println!(
		"{dim}(every setting is a WAYPOINTD_* environment variable; there is no config file){reset}"
	);

	section("Server");
	line(
		"WAYPOINTD_DB_FILE",
		config::db_file().display().to_string(),
		config::DEFAULT_DB_FILE,
	);
	line(
		"WAYPOINTD_DB_CACHE_SIZE",
		config::db_cache_size_kib().to_string(),
		&config::DEFAULT_DB_CACHE_SIZE_KIB.to_string(),
	);
	line(
		"WAYPOINTD_DB_MMAP_SIZE",
		config::db_mmap_size().to_string(),
		&config::DEFAULT_DB_MMAP_SIZE.to_string(),
	);
	line("WAYPOINTD_SERVE_HOST", config::host(), config::DEFAULT_HOST);
	line(
		"WAYPOINTD_SERVE_PORT",
		config::port().to_string(),
		&config::DEFAULT_PORT.to_string(),
	);
	line(
		"WAYPOINTD_WAL_CHECKPOINT_SECS",
		config::wal_checkpoint_secs().to_string(),
		&config::DEFAULT_WAL_CHECKPOINT_SECS.to_string(),
	);
	line(
		"WAYPOINTD_REQUEST_TIMEOUT_SECS",
		config::request_timeout_secs().to_string(),
		&config::DEFAULT_REQUEST_TIMEOUT_SECS.to_string(),
	);
	line(
		"WAYPOINTD_MAX_CONCURRENCY",
		config::max_concurrency().to_string(),
		&config::DEFAULT_MAX_CONCURRENCY.to_string(),
	);

	section("Auth");
	line(
		"WAYPOINTD_SERVE_TOKEN",
		token("WAYPOINTD_SERVE_TOKEN"),
		"unset",
	);
	line(
		"WAYPOINTD_READ_TOKEN",
		token("WAYPOINTD_READ_TOKEN"),
		"unset",
	);
	line(
		"WAYPOINTD_COOKIE_SECURE",
		config::cookie_secure().to_string(),
		"false",
	);

	section("Operations");
	line(
		"WAYPOINTD_BACKUP_DIR",
		config::backup_dir().map_or_else(|| "<unset>".to_string(), |p| p.display().to_string()),
		"unset",
	);
	line(
		"WAYPOINTD_BACKUP_INTERVAL_SECS",
		config::backup_interval_secs().to_string(),
		&config::DEFAULT_BACKUP_INTERVAL_SECS.to_string(),
	);
	line(
		"WAYPOINTD_BACKUP_KEEP",
		config::backup_keep().to_string(),
		&config::DEFAULT_BACKUP_KEEP.to_string(),
	);

	section("Logging");
	line(
		"WAYPOINTD_LOG_LEVEL",
		raw_if_valid("WAYPOINTD_LOG_LEVEL", "info", |s| {
			LogLevel::from_env_str(s).is_some()
		}),
		"info",
	);
	line(
		"WAYPOINTD_LOG_FORMAT",
		raw_if_valid("WAYPOINTD_LOG_FORMAT", "human-readable", |s| {
			LogFormat::from_env_str(s).is_some()
		}),
		"human-readable",
	);
	line(
		"WAYPOINTD_LOG_FILE",
		raw("WAYPOINTD_LOG_FILE", "(stderr)"),
		"(stderr)",
	);

	section("Cache");
	line(
		"WAYPOINTD_CACHE_DIR",
		waypointd::core::cache::cache_dir().display().to_string(),
		"platform cache dir",
	);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	if let Some(action) = cli_action() {
		return cli_dispatch(action);
	}

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
		db_pragmas: waypointd::database::DbPragmas {
			cache_size_kib: config::db_cache_size_kib(),
			mmap_size: config::db_mmap_size(),
		},
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
