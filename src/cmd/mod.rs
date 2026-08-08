/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! CLI layer: clap parsing and dispatch. Commands are grouped under
//! `bookmarks`, `tags`, `categories`, `trash`, `stats`, and `check`; every
//! handler calls into `core` / `database` — the same seam the HTTP layer
//! uses.
//!
//! The grouping matters to users AND to the code: the required groups
//! (`bookmarks`, `tags`, `categories`) are plain struct-variant subcommand
//! enums, while the optional groups (`trash`, `stats`, `check`) get
//! `disable_help_subcommand` so a bare `waypoint trash` behaves like
//! `waypoint trash list` instead of erroring on a missing subcommand.

pub mod bookmarks;
pub mod categories;
pub mod check;
mod output;
pub mod stats;
pub mod tags;
pub mod trash;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

use crate::database;
use crate::logging::{LogFormat, LogLevel};

#[derive(Parser, Debug)]
#[command(
	name = "waypoint",
	version,
	about = "A modern, self-hosted bookmark manager",
	color = clap::ColorChoice::Always
)]
pub struct Cli {
	#[command(subcommand)]
	pub command: Command,

	/// Path to the SQLite database file
	#[arg(
		long,
		short = 'D',
		global = true,
		env = "WAYPOINT_DB_FILE",
		default_value = "waypoint.sqlite",
		help_heading = "Global options"
	)]
	pub database: PathBuf,

	/// Log verbosity (defaults: `warn`, or `info` when running `serve`)
	#[arg(
		long = "log-level",
		short = 'L',
		global = true,
		value_enum,
		help_heading = "Global options"
	)]
	pub log_level: Option<LogLevel>,

	/// Log output format
	#[arg(
		long = "log-format",
		short = 'F',
		global = true,
		value_enum,
		default_value = "pretty",
		help_heading = "Global options"
	)]
	pub log_format: LogFormat,

	/// Write logs to this file instead of stderr
	#[arg(long = "log-file", global = true, help_heading = "Global options")]
	pub log_file: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
	/// Start the web server
	Serve {
		/// Host/interface to bind to
		#[arg(
			long,
			short = 'H',
			env = "WAYPOINT_SERVE_HOST",
			default_value = "localhost"
		)]
		host: String,
		/// Port to listen on
		#[arg(long, short = 'p', env = "WAYPOINT_SERVE_PORT", default_value_t = 8080)]
		port: u16,
		/// Require `Authorization: Bearer <token>` on all `/api/*` requests and
		/// the `/api/openapi.json` docs route. If unset, the API is open. Note
		/// the server speaks plain HTTP, so only use this on a trusted network
		/// or behind a reverse proxy that terminates TLS.
		#[arg(long, env = "WAYPOINT_SERVE_TOKEN")]
		api_token: Option<String>,
		/// Serve the frontend from this directory instead of the embedded
		/// copy. Only exists in debug builds — a release binary always
		/// serves the embedded frontend, so the flag is simply absent there.
		#[cfg(debug_assertions)]
		#[arg(long)]
		static_dir: Option<PathBuf>,
	},
	/// Bookmark operations: add, list, get, update, remove, open, search,
	/// dedup, import, export
	// Boxed: the variant carries the largest of the subcommand enums, and
	// clap supports `Box` here transparently.
	#[command(subcommand, alias = "bk")]
	Bookmarks(Box<bookmarks::Command>),
	/// Tag operations: list, rename, delete
	#[command(subcommand)]
	Tags(tags::Command),
	/// Category operations: list, rename, delete
	#[command(subcommand, alias = "ctg")]
	Categories(categories::Command),
	/// Recycle bin: list and restore trashed bookmarks
	#[command(disable_help_subcommand = true)]
	Trash {
		#[command(subcommand)]
		command: Option<trash::Command>,
	},
	/// Bookmark statistics (bare `waypoint stats` shows the overview)
	#[command(disable_help_subcommand = true)]
	Stats {
		#[command(subcommand)]
		command: Option<stats::Command>,
	},
	/// Find bookmarked sites that no longer exist on the internet
	#[command(disable_help_subcommand = true)]
	Check {
		/// Move dead links to trash (recoverable with `trash restore`)
		#[arg(long, conflicts_with = "hard_delete")]
		delete: bool,
		/// Permanently delete dead links
		#[arg(long, short = 'x')]
		hard_delete: bool,
		/// Number of concurrent checks
		#[arg(long, default_value_t = 8)]
		jobs: usize,
		#[command(subcommand)]
		command: Option<check::Command>,
	},
}

/// Output format for `bookmarks export`
#[derive(Clone, Copy, Debug, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum ExportFormat {
	Md,
	Csv,
}

/// Runs any subcommand except `serve` (which needs the async runtime and
/// is handled directly in `main`). Opens its own connection since these
/// are short-lived, one-shot CLI invocations.
pub fn run_command(db_path: &Path, command: Command) -> Result<()> {
	let conn = database::open(db_path)?;
	crate::log_debug!("opened database at {}", db_path.display());
	crate::log_debug!("dispatching command: {command:?}");

	match command {
		// `serve` is dispatched from `main` before this function is ever
		// called; if we see it here something went wrong upstream.
		Command::Serve { .. } => unreachable!("serve is handled in main"),
		Command::Bookmarks(cmd) => bookmarks::run(&conn, *cmd),
		Command::Tags(cmd) => tags::run(&conn, cmd),
		Command::Categories(cmd) => categories::run(&conn, cmd),
		Command::Trash { command } => trash::run(&conn, command),
		Command::Stats { command } => stats::run(&conn, command),
		Command::Check {
			delete,
			hard_delete,
			jobs,
			command,
		} => check::run(&conn, delete, hard_delete, jobs, command),
	}
}
