/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! `waypoint categories` subcommand: list, rename, and delete categories.
//! Renaming and deleting both go through `database::categories`, which
//! re-parents bookmarks (renames keep them, deletes move them to the
//! default category) — no bookmark data is ever lost by these commands.

use anyhow::Result;
use clap::Subcommand;
use rusqlite::Connection;

use crate::database::categories as db;

#[derive(Subcommand, Debug)]
pub enum Command {
	/// List categories with bookmark counts
	List {
		/// Print as JSON instead of a human-readable list
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// Rename a category (bookmarks move with it)
	Rename {
		/// Category id
		id: i64,
		/// New category name
		name: String,
	},
	/// Delete a category (its bookmarks move to the default category)
	Delete {
		/// Category id
		id: i64,
	},
}

pub fn run(conn: &Connection, command: Command) -> Result<()> {
	match command {
		Command::List { json } => {
			let categories = db::list(conn)?;
			if json {
				// Machine output: full serde serialization of the count list.
				println!("{}", serde_json::to_string_pretty(&categories)?);
			} else {
				// Human output: one line per category, id-padded so the
				// columns line up for single-digit vs multi-digit ids.
				println!("Categories:");
				if categories.is_empty() {
					println!("  (none yet)");
				}
				for c in categories {
					println!("  #{:<4} {}", c.id, c.name);
				}
			}
			Ok(())
		}
		Command::Rename { id, name } => {
			// The boolean return distinguishes "renamed" from "no such
			// category" so the error message can name the real problem
			// instead of printing a generic failure.
			if db::rename(conn, id, &name)? {
				crate::log_info!("renamed category #{id} -> {name:?}");
				println!("renamed category #{id} to \"{name}\"");
			} else {
				crate::log_warn!("no category with id #{id}");
				println!("no category with id #{id}");
			}
			Ok(())
		}
		Command::Delete { id } => {
			if db::delete(conn, id)? {
				crate::log_info!("deleted category #{id} (bookmarks moved to default)");
				println!("deleted category #{id} (bookmarks moved to default)");
			} else {
				crate::log_warn!("no category with id #{id}");
				println!("no category with id #{id}");
			}
			Ok(())
		}
	}
}
