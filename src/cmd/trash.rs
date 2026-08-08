/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! `waypoint trash` subcommand: the recycle bin. Lists trashed bookmarks
//! and restores them. A bare `waypoint trash` (no subcommand) behaves like
//! `waypoint trash list`, mirroring how `check` runs its default action
//! when given no subcommand — hence `command: Option<Command>` in the
//! parent enum instead of a required subcommand.

use anyhow::Result;
use clap::{Args, Subcommand};
use rusqlite::Connection;

use crate::database::bookmarks as db;
use crate::model::BookmarkFilter;
use crate::shared;

use super::output;

/// Normalizes an optional `trash list`/`trash empty` date bound into the
/// fixed-width UTC form `BookmarkFilter` expects (see
/// `shared::parse_datetime_bound`).
fn parse_trashed_bound(value: Option<String>, end_of_day: bool) -> Result<Option<String>> {
	shared::parse_datetime_bound_option(value, end_of_day).map_err(anyhow::Error::msg)
}

/// Trash subcommands. A bare `waypoint trash` (no subcommand) behaves like
/// `trash list`, mirroring how `check` runs its default action when given
/// no subcommand — so the enum is `Option`al in the parent.
#[derive(Subcommand, Debug)]
pub enum Command {
	/// List trashed bookmarks
	List(TrashListArgs),
	/// Restore trashed bookmarks
	Restore {
		/// Bookmark ids to restore
		#[arg(required = true)]
		ids: Vec<i64>,
	},
	/// Permanently delete every trashed bookmark (with `--before`, only
	/// those trashed on or before that UTC date/time)
	Empty {
		/// Only purge bookmarks trashed at or before this UTC date/time
		/// (YYYY-MM-DD[ HH:MM[:SS]])
		#[arg(long)]
		before: Option<String>,
		/// Skip the confirmation prompt
		#[arg(long, short = 'y')]
		yes: bool,
		/// Report the matching ids/count without deleting anything
		#[arg(long)]
		dry_run: bool,
	},
}

#[derive(Args, Debug, Default)]
pub struct TrashListArgs {
	/// Only show bookmarks in this category
	#[arg(long, short = 'c')]
	pub category: Option<String>,
	/// Only show bookmarks in the category with this id
	#[arg(long)]
	pub category_id: Option<i64>,
	/// Only show bookmarks carrying this tag
	#[arg(long, short = 't')]
	pub tag: Option<String>,
	/// Only show bookmarks with this keyword shortcut
	#[arg(long, short = 'k')]
	pub keyword: Option<String>,
	/// Only show starred bookmarks
	#[arg(long, short = 's')]
	pub starred: bool,
	/// Only show bookmarks trashed at or after this UTC date/time
	#[arg(long)]
	pub trashed_after: Option<String>,
	/// Only show bookmarks trashed at or before this UTC date/time
	#[arg(long)]
	pub trashed_before: Option<String>,
	/// Maximum number of bookmarks to show
	#[arg(long, short = 'l', default_value_t = 50)]
	pub limit: i64,
	/// Print as JSON instead of a human-readable list
	#[arg(long, short = 'j')]
	pub json: bool,
	/// Print only the URLs, one per line (for piping into other tools)
	#[arg(long, short = 'u', conflicts_with = "json")]
	pub links: bool,
}

pub fn run(conn: &Connection, command: Option<Command>) -> Result<()> {
	match command {
		// Bare `waypoint trash` behaves like `trash list`, mirroring how
		// `check` runs its default action when given no subcommand.
		None => print_trash(conn, TrashListArgs::default()),
		Some(Command::List(args)) => print_trash(conn, args),
		Some(Command::Restore { ids }) => {
			// Restore is per-id; a mix of existing and stale ids reports
			// each one accurately rather than failing wholesale.
			for id in ids {
				if db::restore(conn, id)? {
					crate::log_info!("restored bookmark #{id}");
					println!("restored bookmark #{id}");
				} else {
					crate::log_warn!("no trashed bookmark with id #{id}");
					println!("no trashed bookmark with id #{id}");
				}
			}
			Ok(())
		}
		Some(Command::Empty {
			before,
			yes,
			dry_run,
		}) => {
			let before = parse_trashed_bound(before, true)?;
			let filter = BookmarkFilter {
				trash: true,
				trashed_before: before.clone(),
				limit: None,
				offset: None,
				..Default::default()
			};
			let ids = db::select_ids(conn, &filter)?;
			if ids.is_empty() {
				crate::log_warn!(
					"trash empty: {}",
					match &before {
						Some(b) => format!("no trashed bookmarks from before {b}"),
						None => "trash is already empty".to_string(),
					}
				);
				println!("trash is already empty");
				return Ok(());
			}
			let scope = match before {
				Some(b) => format!(
					"{count} trashed bookmark(s) from before {b}",
					count = ids.len()
				),
				None => format!("{} trashed bookmark(s)", ids.len()),
			};
			if dry_run {
				crate::log_info!("trash empty dry-run: {scope} would be purged");
				println!("would permanently delete {scope}:");
				for id in &ids {
					println!("  #{id}");
				}
				return Ok(());
			}
			if !yes {
				// Interactive guard: an empty trash is the only case that
				// doesn't prompt, so `--yes` is opt-in for everything else.
				print!("permanently delete {scope}? [y/N] ");
				use std::io::Write;
				std::io::stdout().flush()?;
				let mut answer = String::new();
				std::io::stdin().read_line(&mut answer)?;
				if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
					println!("aborted");
					return Ok(());
				}
			}
			let result = db::remove_matching(conn, &filter, true)?;
			crate::log_info!(
				"trash emptied: permanently deleted {} bookmark(s)",
				result.removed
			);
			println!("permanently deleted {} bookmark(s)", result.removed);
			Ok(())
		}
	}
}

/// Lists the recycle bin. Also used for a bare `waypoint trash`.
pub fn print_trash(conn: &Connection, args: TrashListArgs) -> Result<()> {
	// `trash: true` is the key difference from `bookmarks list`: it flips
	// the WHERE clause from "trashed_at IS NULL" to "IS NOT NULL" and the
	// ordering to most-recently-trashed first (see `database::bookmarks`).
	let trashed_after = parse_trashed_bound(args.trashed_after, false)?;
	let trashed_before = parse_trashed_bound(args.trashed_before, true)?;
	let filter = BookmarkFilter {
		category: args.category,
		category_id: args.category_id,
		tag: args.tag,
		keyword: args.keyword,
		starred: if args.starred { Some(true) } else { None },
		archived: None,
		trash: true,
		trashed_after,
		trashed_before,
		limit: Some(args.limit),
		offset: None,
		..Default::default()
	};
	let bookmarks = db::list(conn, &filter)?;
	crate::log_debug!("listed {} trashed bookmarks", bookmarks.len());
	// `trash: true` here tells the renderer to append the "(in trash)"
	// marker so the recycle bin view doesn't look identical to a live list.
	if args.links {
		for b in &bookmarks {
			println!("{}", b.url);
		}
	} else {
		output::print_bookmarks(&bookmarks, args.json, true)?;
	}
	Ok(())
}
