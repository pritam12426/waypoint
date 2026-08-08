/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! `waypoint check` subcommand: probe every bookmarked site and report (or
//! trash) the ones that are dead. The heavy lifting lives in
//! `core::checker`; this module only maps the CLI surface onto it. A bare
//! `waypoint check` runs the default action (report to stdout) with the
//! `--delete`/`--hard-delete` flags applied; `waypoint check export` writes
//! a report file instead.

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use rusqlite::Connection;

use crate::core::checker::{self, ExportFormat};

#[derive(Subcommand, Debug)]
pub enum Command {
	/// Write a report of dead links to a file
	Export {
		/// Output file path, or `-` for stdout
		#[arg(default_value = "-")]
		file: String,
		/// Output format
		#[arg(value_enum, long, default_value_t = CheckExportFormat::Csv)]
		format: CheckExportFormat,
	},
}

/// A small mirror of `super::ExportFormat` with a CSV default, so the parent
/// enum stays free of a default. Kept separate from `cmd::ExportFormat` so the
/// `check export` default can differ from `bookmarks export`.
#[derive(Clone, Copy, ValueEnum, Debug)]
pub enum CheckExportFormat {
	Csv,
	Md,
}

// Bridge from the CLI-local enum to the checker's own format type. Kept
// here rather than in `core::checker` so the checker stays clap-free.
impl From<CheckExportFormat> for ExportFormat {
	fn from(f: CheckExportFormat) -> Self {
		match f {
			CheckExportFormat::Csv => ExportFormat::Csv,
			CheckExportFormat::Md => ExportFormat::Markdown,
		}
	}
}

pub fn run(
	conn: &Connection,
	delete: bool,
	hard_delete: bool,
	jobs: usize,
	command: Option<Command>,
) -> Result<()> {
	// clap already declares these two flags mutually exclusive, but the
	// same invariant must hold when both arrive through the API — cheap
	// to double-check here so `core::checker` never has to reason about it.
	if delete && hard_delete {
		anyhow::bail!("--delete and --hard-delete are mutually exclusive");
	}
	let spec = command.map(|command| match command {
		Command::Export { file, format } => checker::ExportSpec {
			format: ExportFormat::from(format),
			path: file.into(),
		},
	});

	checker::run(conn, delete, hard_delete, spec, jobs)
}
