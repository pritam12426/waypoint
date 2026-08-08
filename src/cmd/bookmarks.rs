/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! `waypoint bookmarks` subcommand: the ten bookmark operations
//! (add, list, get, update, remove, open, search, dedup, import, export).
//! Each `Command` variant is a thin translation layer — CLI args in,
//! `database::bookmarks` / `core::import_export` calls out, human-readable
//! (or `--json`) output back. Nothing here touches SQL directly.

use anyhow::Result;
use clap::{Args, Subcommand};
use rusqlite::Connection;
use std::path::PathBuf;

use crate::core::import_export;
use crate::database::{bookmarks as db, visits as vis_db};
use crate::model::{AssetMode, BookmarkFilter, NewBookmark, UpdateBookmark};
use crate::shared::{self, validate_id};

use super::{ExportFormat, output};

/// Normalizes an optional `--*-after`/`--*-before` CLI value into the
/// fixed-width UTC form `BookmarkFilter` expects (see
/// `shared::parse_datetime_bound`). Garbage input surfaces as a targeted
/// message via anyhow, not as a raw SQL error.
fn parse_bound(value: Option<String>, end_of_day: bool) -> Result<Option<String>> {
	shared::parse_datetime_bound_option(value, end_of_day).map_err(anyhow::Error::msg)
}

/// Enforces that each normalized `*_after` / `*_before` pair is a sane range
/// (after must not sort after before) — an inverted range is a targeted
/// error rather than a silently-empty result.
fn validate_time_range_pair(
	created: (Option<String>, Option<String>),
	updated: (Option<String>, Option<String>),
	visited: (Option<String>, Option<String>),
) -> Result<()> {
	for (label, (after, before)) in [
		("created", created),
		("updated", updated),
		("last_visited", visited),
	] {
		shared::validate_time_range(after.as_deref(), before.as_deref(), label)
			.map_err(anyhow::Error::msg)?;
	}
	Ok(())
}

#[derive(Subcommand, Debug)]
pub enum Command {
	/// Add a new bookmark
	Add {
		/// The URL to bookmark
		url: String,
		/// Title to display instead of the page's <title>
		#[arg(long)]
		title: Option<String>,
		/// Shortcut keyword served at /keywords/{keyword}
		#[arg(long, short = 'k')]
		keyword: Option<String>,
		/// Category to place the bookmark in (created if missing)
		#[arg(long, short = 'c')]
		category: Option<String>,
		/// Comma-separated list of tags
		#[arg(long, short = 't', value_delimiter = ',')]
		tags: Option<Vec<String>>,
		/// Short description shown in search results
		#[arg(long, short = 'd')]
		description: Option<String>,
		/// Free-form note attached to the bookmark
		#[arg(long, short = 'n')]
		note: Option<String>,
		/// URL of the favicon to display (mutually exclusive with
		/// `--thumbnail`, `--no-custom-favicon`, `--no-thumbnail`)
		#[arg(
			long,
			short = 'f',
			conflicts_with_all = ["thumbnail", "no_custom_favicon", "no_thumbnail"]
		)]
		favicon: Option<String>,
		/// URL of a thumbnail image for the bookmark (mutually exclusive with
		/// `--favicon`, `--no-custom-favicon`, `--no-thumbnail`)
		#[arg(
			long,
			conflicts_with_all = ["favicon", "no_custom_favicon", "no_thumbnail"]
		)]
		thumbnail: Option<String>,
		/// Use the generic domain favicon (scheme://host/favicon.ico)
		/// instead of a site-specific/custom one (mutually exclusive with
		/// `--favicon`, `--thumbnail`, `--no-thumbnail`)
		#[arg(
			long,
			conflicts_with_all = ["favicon", "thumbnail", "no_thumbnail"]
		)]
		no_custom_favicon: bool,
		/// Do not auto-add a thumbnail (e.g. YouTube video thumbnails)
		/// (mutually exclusive with `--favicon`, `--thumbnail`,
		/// `--no-custom-favicon`)
		#[arg(
			long,
			conflicts_with_all = ["favicon", "thumbnail", "no_custom_favicon"]
		)]
		no_thumbnail: bool,
		/// Media resolution mode for favicon/thumbnail: `auto` (derive from
		/// the page, the default), `fetch` (scrape the live page at save
		/// time), or `default` (generic placeholder assets). Explicit
		/// `--favicon`/`--thumbnail` values always win over the mode.
		/// (mutually exclusive with `--favicon`, `--thumbnail`,
		/// `--no-custom-favicon`, `--no-thumbnail`)
		#[arg(
			long,
			value_enum,
			conflicts_with_all = ["favicon", "thumbnail", "no_custom_favicon", "no_thumbnail"]
		)]
		mode: Option<AssetMode>,
		/// Mark the bookmark as starred
		#[arg(long, short = 's')]
		starred: bool,
	},
	/// List bookmarks
	List(ListArgs),
	/// Show full details for one bookmark
	Get {
		/// Bookmark id
		id: i64,
		/// Print as JSON instead of a human-readable detail block
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// Update fields on existing bookmarks
	Update(UpdateArgs),
	/// Remove bookmarks (move to trash by default, recoverable with
	/// `trash restore`). Pass ids and/or filter criteria; with no ids the
	/// criteria decide which bookmarks are removed.
	Remove(RemoveArgs),
	/// Open bookmark URLs in your browser
	Open {
		/// Bookmark ids to open
		#[arg(required = true)]
		ids: Vec<i64>,
		/// Browser to use instead of the system default (e.g. Safari, Brave)
		#[arg(long, short = 'b')]
		browser: Option<String>,
	},
	/// Full-text search across titles, descriptions, notes, and URLs
	Search {
		/// Search text (matches titles, descriptions, notes, and URLs)
		query: String,
		/// Search the archive (archived bookmarks) instead of active ones
		#[arg(long, short = 'a')]
		archived: bool,
		/// Only show results in this category
		#[arg(long, short = 'c')]
		category: Option<String>,
		/// Only show results carrying this tag
		#[arg(long, short = 't')]
		tag: Option<String>,
		/// Only show results with this keyword shortcut
		#[arg(long, short = 'k')]
		keyword: Option<String>,
		/// Maximum number of results to show
		#[arg(long, short = 'l', default_value_t = 20)]
		limit: i64,
		/// Print as JSON instead of a human-readable list
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// Find and merge duplicate bookmarks (same URL)
	Dedup {
		/// Show duplicates without modifying anything
		#[arg(long)]
		dry_run: bool,
		/// Permanently delete duplicates instead of moving to trash
		#[arg(long)]
		purge: bool,
	},
	/// Import bookmarks from a Netscape bookmark HTML file — the format
	/// every major browser exports to (Chrome/Firefox "Export bookmarks",
	/// Safari "File > Export > Bookmarks"). Accepts a `.html` file such as
	/// `bookmarks.html` or `bookmarks_2024-08-01.html`; `<H3>` folder
	/// headings become categories.
	Import {
		/// Netscape bookmark HTML file to read (e.g. bookmarks.html)
		file: PathBuf,
		/// Comma-separated tags to apply to every imported bookmark
		#[arg(long, short = 't', value_delimiter = ',')]
		tag: Option<Vec<String>>,
		/// Category for every imported bookmark, overriding folder-derived
		/// categories (created if missing)
		#[arg(long, short = 'c')]
		category: Option<String>,
		/// Import bookmarks straight into the archive
		#[arg(long, short = 'a')]
		archive: bool,
	},
	/// Export bookmarks to a file
	Export {
		/// File to write the export to, or `-` to print to stdout
		file: PathBuf,
		/// Output format
		#[arg(long, value_enum, default_value = "md")]
		format: ExportFormat,
	},
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
	/// Bookmark ids to remove (optional when filter criteria are given)
	#[arg(num_args = 0..)]
	pub ids: Vec<i64>,
	/// Permanently delete instead of moving to trash
	#[arg(long)]
	pub purge: bool,
	/// Preview which bookmarks match (ids + count) without removing anything
	#[arg(long)]
	pub dry_run: bool,
	/// Only remove bookmarks in this category
	#[arg(long, short = 'c', conflicts_with = "category_id")]
	pub category: Option<String>,
	/// Only remove bookmarks in the category with this id
	#[arg(long)]
	pub category_id: Option<i64>,
	/// Only remove bookmarks carrying this tag
	#[arg(long, short = 't')]
	pub tag: Option<String>,
	/// Only remove bookmarks with this keyword shortcut
	#[arg(long, short = 'k')]
	pub keyword: Option<String>,
	/// Only remove bookmarks created at or after this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub created_after: Option<String>,
	/// Only remove bookmarks created at or before this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub created_before: Option<String>,
	/// Only remove bookmarks updated at or after this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub updated_after: Option<String>,
	/// Only remove bookmarks updated at or before this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub updated_before: Option<String>,
	/// Only remove bookmarks last visited at or after this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub visited_after: Option<String>,
	/// Only remove bookmarks last visited at or before this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub visited_before: Option<String>,
}

#[derive(Args, Debug)]
pub struct ListArgs {
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
	/// Only show bookmarks created at or after this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub created_after: Option<String>,
	/// Only show bookmarks created at or before this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub created_before: Option<String>,
	/// Only show bookmarks updated at or after this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub updated_after: Option<String>,
	/// Only show bookmarks updated at or before this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub updated_before: Option<String>,
	/// Only show bookmarks last visited at or after this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub visited_after: Option<String>,
	/// Only show bookmarks last visited at or before this UTC date/time
	/// (YYYY-MM-DD[ HH:MM[:SS]])
	#[arg(long)]
	pub visited_before: Option<String>,
	/// Only show archived bookmarks
	#[arg(long, short = 'a', conflicts_with = "all")]
	pub archived: bool,
	/// Show both active and archived bookmarks
	#[arg(long, short = 'e', conflicts_with = "archived")]
	pub all: bool,
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

#[derive(Args, Debug)]
pub struct UpdateArgs {
	/// Bookmark ids to update
	#[arg(required = true)]
	pub ids: Vec<i64>,
	/// New title to display instead of the page's <title>
	#[arg(long)]
	pub title: Option<String>,
	/// New URL for the bookmark (re-derives the favicon/thumbnail on change)
	#[arg(long)]
	pub url: Option<String>,
	/// New keyword shortcut served at /keywords/{keyword}
	#[arg(long, short = 'k', conflicts_with = "clear_keyword")]
	pub keyword: Option<String>,
	/// Remove the keyword shortcut from this bookmark
	#[arg(long)]
	pub clear_keyword: bool,
	/// Move the bookmark to this category (created if missing)
	#[arg(long, short = 'c')]
	pub category: Option<String>,
	/// Comma-separated list of tags (replaces the existing tag set)
	#[arg(long, short = 't', value_delimiter = ',', conflicts_with_all = ["add_tags", "remove_tags"])]
	pub tags: Option<Vec<String>>,
	/// Comma-separated tags to add (without touching existing tags)
	#[arg(long, value_delimiter = ',')]
	pub add_tags: Option<Vec<String>>,
	/// Comma-separated tags to remove
	#[arg(long, value_delimiter = ',')]
	pub remove_tags: Option<Vec<String>>,
	/// New description shown in search results
	#[arg(long, short = 'd')]
	pub description: Option<String>,
	/// New free-form note attached to the bookmark
	#[arg(long, short = 'n')]
	pub note: Option<String>,
	/// URL of the favicon to display (mutually exclusive with
	/// `--thumbnail`, `--no-custom-favicon`, `--no-thumbnail`)
	#[arg(
		long,
		short = 'f',
		conflicts_with_all = ["thumbnail", "no_custom_favicon", "no_thumbnail"]
	)]
	pub favicon: Option<String>,
	/// URL of a thumbnail image for the bookmark (mutually exclusive with
	/// `--favicon`, `--no-custom-favicon`, `--no-thumbnail`)
	#[arg(
		long,
		conflicts_with_all = ["favicon", "no_custom_favicon", "no_thumbnail"]
	)]
	pub thumbnail: Option<String>,
	/// Reset the favicon to the generic domain favicon (scheme://host/favicon.ico)
	/// (mutually exclusive with `--favicon`, `--thumbnail`, `--no-thumbnail`)
	#[arg(
		long,
		conflicts_with_all = ["favicon", "thumbnail", "no_thumbnail"]
	)]
	pub no_custom_favicon: bool,
	/// Clear the thumbnail (and don't re-derive it on a URL change)
	/// (mutually exclusive with `--favicon`, `--thumbnail`, `--no-custom-favicon`)
	#[arg(
		long,
		conflicts_with_all = ["favicon", "thumbnail", "no_custom_favicon"]
	)]
	pub no_thumbnail: bool,
	/// Media resolution mode for favicon/thumbnail (see `add --mode`).
	/// Applies to fields without an explicit `--favicon`/`--thumbnail`/
	/// sentinel value in this invocation. (mutually exclusive with
	/// `--favicon`, `--thumbnail`, `--no-custom-favicon`, `--no-thumbnail`)
	#[arg(
		long,
		value_enum,
		conflicts_with_all = ["favicon", "thumbnail", "no_custom_favicon", "no_thumbnail"]
	)]
	pub mode: Option<AssetMode>,
	/// Re-fetch the favicon and thumbnail from the page now, bypassing the
	/// fetched-media cache (which normally reuses successful results for
	/// 90 days). Explicit `--mode` / `--favicon` / `--thumbnail` / `--no-*`
	/// values in this invocation still win.
	#[arg(long, short = 'R')]
	pub refresh: bool,
	/// Mark the bookmark as starred
	#[arg(long, short = 's', conflicts_with = "unstar")]
	pub star: bool,
	/// Remove the star from the bookmark
	#[arg(long, short = 'r')]
	pub unstar: bool,
	/// Archive the bookmark (excluded from default search and listing)
	#[arg(long, short = 'a', conflicts_with = "unarchive")]
	pub archive: bool,
	/// Restore an archived bookmark to the active set
	#[arg(long)]
	pub unarchive: bool,
}

pub fn run(conn: &Connection, command: Command) -> Result<()> {
	match command {
		Command::Add {
			url,
			title,
			keyword,
			category,
			tags,
			description,
			note,
			favicon,
			thumbnail,
			no_custom_favicon,
			no_thumbnail,
			mode,
			starred,
		} => {
			// The `--no-*` flags map to the model's empty-string sentinel:
			// `""` favicon → generic domain favicon, `""` thumbnail → none.
			// clap's `conflicts_with` guarantees they can't meet an explicit
			// `--favicon`/`--thumbnail` in the same invocation.
			let favicon = if no_custom_favicon {
				crate::log_info!("favicon media auto-resolution disabled (--no-custom-favicon)");
				Some(String::new())
			} else {
				favicon
			};
			let thumbnail = if no_thumbnail {
				crate::log_info!("thumbnail auto-resolution disabled (--no-thumbnail)");
				Some(String::new())
			} else {
				thumbnail
			};
			let new = NewBookmark {
				url,
				title,
				description,
				category,
				tags,
				keyword,
				note,
				favicon,
				thumbnail,
				favicon_mode: mode,
				thumbnail_mode: mode,
				starred: Some(starred),
				is_archived: None,
			};
			let id = db::insert(conn, &new)?;
			if let Some(mode) = mode {
				crate::log_info!("bookmark #{id} media mode: {mode}");
			}
			crate::log_info!("added bookmark #{id}");
			println!("added bookmark #{id}");
			Ok(())
		}

		Command::List(args) => {
			let ListArgs {
				category,
				category_id,
				tag,
				keyword,
				starred,
				created_after,
				created_before,
				updated_after,
				updated_before,
				visited_after,
				visited_before,
				archived,
				all,
				limit,
				json,
				links,
			} = args;
			// Tri-state archived: `--all` means "don't filter on archived at
			// all" (None); `--archived` filters to archived (Some(true));
			// the default filters to active (Some(false)). clap's
			// `conflicts_with` guarantees `archived` and `all` can't both
			// be passed, so this ordering is unambiguous.
			let archived_filter = if all {
				None
			} else if archived {
				Some(true)
			} else {
				Some(false)
			};
			// Time bounds go through `shared::parse_datetime_bound`, which
			// normalizes bare dates to day-start (after) or day-end (before)
			// so a `--created-after 2026-01-01` is inclusive of that whole
			// day. Garbage input fails here with a targeted message instead
			// of a SQL error.
			let created_after = parse_bound(created_after, false)?;
			let created_before = parse_bound(created_before, true)?;
			let updated_after = parse_bound(updated_after, false)?;
			let updated_before = parse_bound(updated_before, true)?;
			let visited_after = parse_bound(visited_after, false)?;
			let visited_before = parse_bound(visited_before, true)?;
			validate_time_range_pair(
				(created_after.clone(), created_before.clone()),
				(updated_after.clone(), updated_before.clone()),
				(visited_after.clone(), visited_before.clone()),
			)?;
			let filter = BookmarkFilter {
				category,
				category_id,
				tag,
				keyword,
				starred: if starred { Some(true) } else { None },
				archived: archived_filter,
				trash: false,
				created_after,
				created_before,
				updated_after,
				updated_before,
				last_visited_after: visited_after,
				last_visited_before: visited_before,
				limit: Some(limit),
				offset: None,
				..Default::default()
			};
			let bookmarks = db::list(conn, &filter)?;
			crate::log_debug!("listed {} bookmarks", bookmarks.len());
			if links {
				// `--links` is a script-friendly mode: URLs only, one per
				// line, no ANSI and no headers — safe to pipe into xargs.
				for b in &bookmarks {
					println!("{}", b.url);
				}
			} else {
				output::print_bookmarks(&bookmarks, json, false)?;
			}
			Ok(())
		}

		Command::Get { id, json } => {
			// Ids are validated as positive integers; `validate_id` returns
			// an Err for <= 0, which surfaces as a clean CLI error.
			let id = validate_id(id).map_err(anyhow::Error::msg)?;
			match db::get(conn, id)? {
				Some(b) => {
					if json {
						println!("{}", serde_json::to_string_pretty(&b)?);
					} else {
						output::print_bookmark_detail(&b, &output::Paint::new());
					}
					Ok(())
				}
				None => {
					// Not found is a normal outcome for a CLI, not an
					// error: print a message and exit successfully so
					// scripts can grep for it without a non-zero status.
					crate::log_warn!("no bookmark with id #{id}");
					println!("no bookmark with id #{id}");
					Ok(())
				}
			}
		}

		Command::Update(args) => {
			let UpdateArgs {
				ids,
				title,
				url,
				keyword,
				clear_keyword,
				category,
				tags,
				add_tags,
				remove_tags,
				description,
				note,
				favicon,
				thumbnail,
				no_custom_favicon,
				no_thumbnail,
				mode,
				refresh,
				star,
				unstar,
				archive,
				unarchive,
			} = args;
			// Two flag pairs collapse into the tri-state fields the model
			// uses: `--clear-keyword` becomes `Some("")` (the "clear"
			// sentinel in `UpdateBookmark`), and `--star`/`--unstar` plus
			// `--archive`/`--unarchive` become `Some(true)`/`Some(false)`.
			// clap's `conflicts_with` ensures each pair's flags can't both
			// be given.
			let keyword = if clear_keyword {
				Some(String::new())
			} else {
				keyword
			};
			let starred = if star {
				Some(true)
			} else if unstar {
				Some(false)
			} else {
				None
			};
			let is_archived = if archive {
				Some(true)
			} else if unarchive {
				Some(false)
			} else {
				None
			};
			// Same empty-string sentinel as add: `--no-custom-favicon`
			// resets to the generic favicon, `--no-thumbnail` clears.
			let favicon = if no_custom_favicon {
				crate::log_info!(
					"reset favicon to the generic domain favicon (--no-custom-favicon)"
				);
				Some(String::new())
			} else {
				favicon
			};
			let thumbnail = if no_thumbnail {
				crate::log_info!("thumbnail cleared / not re-derived (--no-thumbnail)");
				Some(String::new())
			} else {
				thumbnail
			};
			let update = UpdateBookmark {
				title,
				url,
				description,
				category,
				tags,
				add_tags,
				remove_tags,
				keyword,
				note,
				favicon,
				thumbnail,
				favicon_mode: mode,
				thumbnail_mode: mode,
				refresh,
				starred,
				is_archived,
			};
			for id in ids {
				let existing = db::update(conn, id, &update)?;
				if let Some(existing) = existing {
					// `describe` computes a human-readable change list from
					// the pre-update bookmark (e.g. "title: A -> B"), and
					// an empty list means the update was a no-op.
					let changes = update.describe(&existing);
					let changes = if changes.is_empty() {
						"no changes".to_string()
					} else {
						changes.join(", ")
					};
					crate::log_info!("updated bookmark #{id} ({changes})");
					println!("updated bookmark #{id} ({changes})");
				} else {
					crate::log_warn!("no active bookmark with id #{id}");
					println!("no active bookmark with id #{id}");
				}
			}
			Ok(())
		}

		Command::Remove(args) => {
			let RemoveArgs {
				ids,
				purge,
				dry_run,
				category,
				category_id,
				tag,
				keyword,
				created_after,
				created_before,
				updated_after,
				updated_before,
				visited_after,
				visited_before,
			} = args;
			// Criterion bookkeeping: empty criteria means the invocation
			// must carry explicit ids; mixing ids with criteria is refused
			// so a stray `--tag` can't silently gut a whole category.
			let has_criteria = [
				category.is_some(),
				category_id.is_some(),
				tag.is_some(),
				keyword.is_some(),
				created_after.is_some(),
				created_before.is_some(),
				updated_after.is_some(),
				updated_before.is_some(),
				visited_after.is_some(),
				visited_before.is_some(),
			]
			.iter()
			.any(|c| *c);
			if ids.is_empty() && !has_criteria {
				anyhow::bail!(
					"remove needs at least one bookmark id or one criterion \
					 (--category, --category-id, --tag, --keyword, a --*-after/--*-before bound)"
				);
			}
			if !ids.is_empty() && has_criteria {
				anyhow::bail!("remove accepts either bookmark ids or criteria, not both");
			}

			if has_criteria {
				// Time bounds are validated and normalized here so a bare
				// date means the whole UTC day (inclusive), matching list.
				let created_after = parse_bound(created_after, false)?;
				let created_before = parse_bound(created_before, true)?;
				let updated_after = parse_bound(updated_after, false)?;
				let updated_before = parse_bound(updated_before, true)?;
				let visited_after = parse_bound(visited_after, false)?;
				let visited_before = parse_bound(visited_before, true)?;
				validate_time_range_pair(
					(created_after.clone(), created_before.clone()),
					(updated_after.clone(), updated_before.clone()),
					(visited_after.clone(), visited_before.clone()),
				)?;
				let filter = BookmarkFilter {
					category,
					category_id,
					tag,
					keyword,
					starred: None,
					archived: None,
					trash: false,
					created_after,
					created_before,
					updated_after,
					updated_before,
					last_visited_after: visited_after,
					last_visited_before: visited_before,
					limit: None,
					offset: None,
					..Default::default()
				};
				if dry_run {
					let ids = db::select_ids(conn, &filter)?;
					crate::log_info!(
						"dry-run: {} bookmark(s) match the criteria (purge={purge})",
						ids.len()
					);
					if purge {
						println!("would purge {} bookmark(s): {}", ids.len(), fmt_ids(&ids));
					} else {
						println!("would trash {} bookmark(s): {}", ids.len(), fmt_ids(&ids));
					}
				} else {
					let result = db::remove_matching(conn, &filter, purge)?;
					let action = if purge { "purged" } else { "trashed" };
					crate::log_info!(
						"{} {action} {} bookmark(s) matching the criteria",
						if purge {
							"Permanently"
						} else {
							"Moved to trash:"
						},
						result.removed
					);
					println!(
						"{action} {} bookmark(s): {}",
						result.removed,
						fmt_ids(&result.ids)
					);
				}
				return Ok(());
			}

			// Soft delete (`db::trash`) is the default; `--purge` escalates
			// to `db::purge`, which skips the recycle bin entirely.
			for id in ids {
				let ok = if purge {
					db::purge(conn, id)?
				} else {
					db::trash(conn, id)?
				};
				if ok {
					let action = if purge {
						format!("purged bookmark #{id}")
					} else {
						format!("moved bookmark #{id} to trash")
					};
					crate::log_info!("{action}");
					println!("{action}");
				} else {
					crate::log_warn!("no active bookmark with id #{id}");
					println!("no active bookmark with id #{id}");
				}
			}
			Ok(())
		}

		Command::Open { ids, browser } => {
			// Collect id+url pairs first so a missing id can't abort the
			// batch: the ids that exist are opened, the rest are reported.
			let mut to_open = Vec::new();
			for id in &ids {
				match db::get(conn, *id)? {
					Some(b) => to_open.push((b.id, b.url)),
					None => {
						crate::log_warn!("no bookmark with id #{id}");
						println!("no bookmark with id #{id}");
					}
				}
			}
			if to_open.is_empty() {
				return Ok(());
			}

			for (id, url) in &to_open {
				// `--browser` overrides the system default via
				// `open::with_command`; otherwise defer to `open::that`,
				// which respects the OS default browser.
				if let Some(browser) = &browser {
					open::with_command(url, browser).status()?;
					crate::log_info!("opened {url} in browser {browser:?}");
				} else {
					open::that(url)?;
					crate::log_info!("opened {url}");
				}
				println!("opened {url}");
				// Every successful open is a visit, mirroring the HTTP
				// `/open/{id}` redirect.
				vis_db::record(conn, *id)?;
			}
			Ok(())
		}

		Command::Search {
			query,
			archived,
			limit,
			json,
			category,
			tag,
			keyword,
		} => {
			// `--archived` flips which FTS index is queried — the archive
			// index holds only archived, non-trashed bookmarks (see
			// `database::bookmarks::search_archived`).
			let filter = BookmarkFilter {
				category,
				tag,
				keyword,
				..Default::default()
			};
			let bookmarks = if archived {
				db::search_archived(conn, &query, limit, &filter)?
			} else {
				db::search(conn, &query, limit, &filter)?
			};
			crate::log_debug!(
				"search for \"{query}\"{} returned {} results",
				if archived { " (archive)" } else { "" },
				bookmarks.len()
			);
			if bookmarks.is_empty() && !json {
				println!("no matches for \"{query}\"");
			} else {
				output::print_bookmarks(&bookmarks, json, false)?;
			}
			Ok(())
		}

		Command::Dedup { dry_run, purge } => {
			let groups = db::find_duplicates(conn)?;
			if groups.is_empty() {
				println!("no duplicate URLs found");
				return Ok(());
			}
			let total_remove: usize = groups.iter().map(|g| g.remove_ids.len()).sum();
			println!(
				"found {} duplicate URL{} ({} bookmark{} to remove)",
				groups.len(),
				if groups.len() == 1 { "" } else { "s" },
				total_remove,
				if total_remove == 1 { "" } else { "s" },
			);
			for group in &groups {
				println!("\n  {} (keep #{})", group.url, group.keep_id);
				for id in &group.remove_ids {
					// Three modes, one loop: `--dry-run` only narrates,
					// `--purge` hard-deletes, and the default moves the
					// duplicates to trash (recoverable).
					if dry_run {
						println!("    would remove #{id}");
					} else if purge {
						db::purge(conn, *id)?;
						println!("    purged #{id}");
					} else {
						db::trash(conn, *id)?;
						println!("    moved #{id} to trash");
					}
				}
			}
			if dry_run {
				println!("\n(no changes — rerun without --dry-run to apply)");
			} else {
				crate::log_info!("dedup: removed {total_remove} duplicate bookmark(s)");
			}
			Ok(())
		}

		Command::Import {
			file,
			tag,
			category,
			archive,
		} => import_export::import_html(conn, &file, tag, category, archive),

		Command::Export { file, format } => match format {
			ExportFormat::Md => import_export::export_markdown(conn, &file),
			ExportFormat::Csv => import_export::export_csv(conn, &file),
		},
	}
}

/// Compact id-list for removal output: `#1, #2, #3` (or `(none)` when the
/// match was empty).
fn fmt_ids(ids: &[i64]) -> String {
	if ids.is_empty() {
		return "(none)".into();
	}
	ids.iter()
		.map(|id| format!("#{id}"))
		.collect::<Vec<_>>()
		.join(", ")
}

#[cfg(test)]
mod tests {
	use crate::cmd::Cli;
	use clap::Parser;

	const URL: &str = "https://example.com/page";

	/// argv for `waypoint bookmarks add` with the given extra args.
	fn add_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
		let mut args = vec!["waypoint", "bookmarks", "add"];
		args.extend_from_slice(extra);
		args.push(URL);
		args
	}

	/// argv for `waypoint bookmarks update 1` with the given extra args.
	fn update_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
		let mut args = vec!["waypoint", "bookmarks", "update", "1"];
		args.extend_from_slice(extra);
		args
	}

	/// Expands `(flag, value)` pairs into flat argv tokens; `None` value
	/// means a bare `--no-*` switch.
	fn tokens<'a>(pairs: &[(&'a str, Option<&'a str>)]) -> Vec<&'a str> {
		let mut out = Vec::new();
		for (flag, value) in pairs {
			out.push(*flag);
			if let Some(value) = value {
				out.push(*value);
			}
		}
		out
	}

	// The four media flags (`--favicon`, `--thumbnail`, `--no-custom-favicon`,
	// `--no-thumbnail`) must be mutually exclusive: every pair is rejected by
	// clap on both `add` and `update`.
	#[test]
	fn media_flags_are_mutually_exclusive() {
		let pairs: &[&[(&str, Option<&str>)]] = &[
			&[
				("--favicon", Some("https://x/f.png")),
				("--thumbnail", Some("https://x/t.png")),
			],
			&[
				("--favicon", Some("https://x/f.png")),
				("--no-custom-favicon", None),
			],
			&[
				("--favicon", Some("https://x/f.png")),
				("--no-thumbnail", None),
			],
			&[
				("--thumbnail", Some("https://x/t.png")),
				("--no-custom-favicon", None),
			],
			&[
				("--thumbnail", Some("https://x/t.png")),
				("--no-thumbnail", None),
			],
			&[("--no-custom-favicon", None), ("--no-thumbnail", None)],
		];
		for pair in pairs {
			let extra = tokens(pair);
			assert!(
				Cli::try_parse_from(add_args(&extra)).is_err(),
				"add {extra:?} must be rejected"
			);
			assert!(
				Cli::try_parse_from(update_args(&extra)).is_err(),
				"update {extra:?} must be rejected"
			);
		}
	}

	// Each media flag on its own is a valid invocation.
	#[test]
	fn each_media_flag_alone_is_accepted() {
		let singles: &[&[(&str, Option<&str>)]] = &[
			&[("--favicon", Some("https://x/f.png"))],
			&[("--thumbnail", Some("https://x/t.png"))],
			&[("--no-custom-favicon", None)],
			&[("--no-thumbnail", None)],
		];
		for single in singles {
			let extra = tokens(single);
			Cli::try_parse_from(add_args(&extra)).unwrap_or_else(|e| panic!("add {extra:?}: {e}"));
			Cli::try_parse_from(update_args(&extra))
				.unwrap_or_else(|e| panic!("update {extra:?}: {e}"));
		}
	}

	// `--refresh` is accepted on its own (and alongside `--mode`, which
	// still outranks it for any field it explicitly targets).
	#[test]
	fn refresh_flag_is_accepted_and_wired() {
		Cli::try_parse_from(update_args(&["--refresh"])).unwrap_or_else(|e| panic!("{e}"));
		Cli::try_parse_from(update_args(&["--refresh", "--mode", "fetch"]))
			.unwrap_or_else(|e| panic!("{e}"));
		let cli = Cli::try_parse_from(update_args(&["--refresh"])).unwrap();
		match cli.command {
			crate::cmd::Command::Bookmarks(cmd) => match *cmd {
				crate::cmd::bookmarks::Command::Update(args) => assert!(args.refresh),
				_ => panic!("expected update command"),
			},
			_ => panic!("expected bookmarks command"),
		}
	}
}
