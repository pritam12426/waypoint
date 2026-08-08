/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! `waypoint tags` subcommand: list (with bookmark counts), rename, and
//! delete tags. Tags are many-to-many with bookmarks via `bookmark_tags`;
//! renaming a tag here renames it everywhere it is used, and deleting a tag
//! removes it from every bookmark in one operation.

use anyhow::Result;
use clap::Subcommand;
use rusqlite::Connection;

use crate::database::tags as db;

#[derive(Subcommand, Debug)]
pub enum Command {
	/// List tags with bookmark counts
	List {
		/// Print as JSON instead of a human-readable list
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// Rename a tag everywhere it is used
	Rename {
		/// Current tag name
		old: String,
		/// New tag name
		new: String,
	},
	/// Delete a tag (removes it from every bookmark)
	Delete {
		/// Tag name
		name: String,
	},
}

pub fn run(conn: &Connection, command: Command) -> Result<()> {
	match command {
		Command::List { json } => {
			let tags = db::list_with_counts(conn, None, 0)?;
			crate::log_debug!("listed {} tags", tags.len());
			if json {
				println!("{}", serde_json::to_string_pretty(&tags)?);
			} else {
				// Human output caps at 30 rows to keep the terminal from
				// flooding; JSON output stays complete for machines.
				println!("Tags:");
				if tags.is_empty() {
					println!("  (none yet)");
				}
				for t in tags.iter().take(30) {
					println!("  {:<20} {}", t.name, t.count);
				}
			}
			Ok(())
		}
		Command::Rename { old, new } => {
			// `db::rename` returns false when `old` doesn't exist — the
			// distinguishing branch decides the message either way.
			if db::rename(conn, &old, &new)? {
				crate::log_info!("renamed tag {old:?} -> {new:?}");
				println!("renamed tag \"{old}\" to \"{new}\"");
			} else {
				crate::log_warn!("no tag named {old:?}");
				println!("no tag named \"{old}\"");
			}
			Ok(())
		}
		Command::Delete { name } => {
			if db::delete(conn, &name)? {
				crate::log_info!("deleted tag {name:?}");
				println!("deleted tag \"{name}\"");
			} else {
				crate::log_warn!("no tag named {name:?}");
				println!("no tag named \"{name}\"");
			}
			Ok(())
		}
	}
}
