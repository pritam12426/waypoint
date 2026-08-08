/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! `waypoint stats` subcommand: read-only analysis of the library. The
//! overview (a bare `waypoint stats`) is the headline, with dedicated
//! subcommands for domains, tags, keyword shortcuts, visit stats, hygiene,
//! and monthly activity. All queries live in `database::stats` / `visits` /
//! `tags`; every command also has a `--json` mode for scripting.

use anyhow::Result;
use clap::Subcommand;
use rusqlite::Connection;

use crate::database::{bookmarks as db, stats as db_stats, tags, visits};
use crate::model::BookmarkFilter;

use super::output;

#[derive(Subcommand, Debug)]
pub enum Command {
	/// Aggregate overview: totals, category breakdown, top domains/tags/most-visited
	Overview {
		/// Print as JSON
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// Top domains by bookmark count
	Domains {
		/// Maximum number of rows to show
		#[arg(long, short = 'l', default_value_t = 15)]
		limit: usize,
		/// Number of rows to skip
		#[arg(long, short = 'o', default_value_t = 0)]
		offset: usize,
		/// Print as JSON
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// Tag usage counts
	Tags {
		/// Maximum number of rows to show
		#[arg(long, short = 'l', default_value_t = 30)]
		limit: usize,
		/// Number of rows to skip
		#[arg(long, short = 'o', default_value_t = 0)]
		offset: usize,
		/// Print as JSON
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// Detailed info for specific bookmark(s)
	Ids {
		/// Bookmark IDs to inspect
		#[arg(required = true)]
		ids: Vec<i64>,
		/// Print as JSON
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// List bookmarks that have a keyword shortcut
	Keywords {
		/// Prepend the bookmark id to each keyword
		#[arg(long)]
		with_id: bool,
		/// Also print the URL each keyword points to
		#[arg(long, short = 'v')]
		with_values: bool,
		/// Maximum number of bookmarks to show
		#[arg(long, short = 'l', default_value_t = 50)]
		limit: i64,
		/// Only show bookmarks in this category
		#[arg(long, short = 'c')]
		category: Option<String>,
		/// Only show bookmarks carrying this tag
		#[arg(long, short = 't')]
		tag: Option<String>,
		/// Only show starred bookmarks
		#[arg(long, short = 's')]
		starred: bool,
		/// Only show archived bookmarks
		#[arg(long, short = 'a')]
		archived: bool,
	},
	/// Most-visited domains ranked by total visit count across all bookmarks
	TopVisited {
		/// Maximum number of rows to show
		#[arg(long, short = 'l', default_value_t = 20)]
		limit: usize,
		/// Number of rows to skip
		#[arg(long, short = 'o', default_value_t = 0)]
		offset: usize,
		/// Print as JSON
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// Bookmarks that have never been visited via a keyword shortcut
	NeverVisited {
		/// Maximum number of rows to show
		#[arg(long, short = 'l', default_value_t = 50)]
		limit: usize,
		/// Number of rows to skip
		#[arg(long, short = 'o', default_value_t = 0)]
		offset: usize,
		/// Print as JSON
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// Tags that are applied to only one bookmark
	OrphanTags {
		/// Maximum number of rows to show
		#[arg(long, short = 'l', default_value_t = 50)]
		limit: usize,
		/// Number of rows to skip
		#[arg(long, short = 'o', default_value_t = 0)]
		offset: usize,
		/// Print as JSON
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// How many bookmarks are missing tags, notes, or descriptions
	Hygiene {
		/// Print as JSON
		#[arg(long, short = 'j')]
		json: bool,
	},
	/// Bookmarks added per month over the last 12 months
	Activity {
		/// Maximum number of rows to show
		#[arg(long, short = 'l', default_value_t = 12)]
		limit: usize,
		/// Number of rows to skip
		#[arg(long, short = 'o', default_value_t = 0)]
		offset: usize,
		/// Print as JSON
		#[arg(long, short = 'j')]
		json: bool,
	},
}

pub fn run(conn: &Connection, command: Option<Command>) -> Result<()> {
	// One Paint for the whole command so every non-JSON branch shares the
	// same TTY detection.
	let paint = output::Paint::new();
	match command {
		// Bare `waypoint stats` shows the overview.
		None => print_overview(conn, false),
		Some(Command::Overview { json }) => print_overview(conn, json),
		Some(Command::Domains {
			limit,
			offset,
			json,
		}) => {
			let domains = visits::domain_counts(conn, limit, offset)?;
			if json {
				println!("{}", serde_json::to_string_pretty(&domains)?);
			} else {
				println!("{}", paint.bold("Top domains:"));
				if domains.is_empty() {
					println!("  {}", paint.dim("(none yet)"));
				}
				for d in &domains {
					println!(
						"  {} {}",
						paint.cyan(&format!("{:<32}", d.domain)),
						paint.bold(&d.count.to_string())
					);
				}
			}
			Ok(())
		}
		Some(Command::Tags {
			limit,
			offset,
			json,
		}) => {
			let tags = tags::list_with_counts(conn, Some(limit), offset)?;
			if json {
				println!("{}", serde_json::to_string_pretty(&tags)?);
			} else {
				println!("{}", paint.bold("Tags:"));
				if tags.is_empty() {
					println!("  {}", paint.dim("(none yet)"));
				}
				for t in &tags {
					println!(
						"  {} {}",
						paint.magenta(&format!("{:<20}", t.name)),
						paint.bold(&t.count.to_string())
					);
				}
			}
			Ok(())
		}
		Some(Command::Ids { ids, json }) => {
			let bookmarks = db::get_by_ids(conn, &ids)?;
			if bookmarks.is_empty() {
				println!("no bookmarks found for the given ids");
			} else if json {
				println!("{}", serde_json::to_string_pretty(&bookmarks)?);
			} else {
				let paint = output::Paint::new();
				for b in &bookmarks {
					output::print_bookmark_detail(b, &paint);
				}
			}
			Ok(())
		}
		Some(Command::Keywords {
			with_id,
			with_values,
			limit,
			category,
			tag,
			starred,
			archived,
		}) => {
			let filter = BookmarkFilter {
				category,
				tag,
				starred: if starred { Some(true) } else { None },
				// Keywords can point at archived bookmarks too, so the
				// filter still applies; the default restricts to active.
				archived: if archived { Some(true) } else { Some(false) },
				trash: false,
				limit: Some(limit),
				offset: None,
				..Default::default()
			};
			let bookmarks = db::list_keywords(conn, &filter)?;
			crate::log_debug!("listed {} keywords", bookmarks.len());
			// Align the keyword column: compute the widest keyword first,
			// then right-pad every row to it with the `width` format spec.
			let width = bookmarks
				.iter()
				.map(|b| b.keyword.as_deref().unwrap_or_default().len())
				.max()
				.unwrap_or(0);
			for b in &bookmarks {
				let keyword = b.keyword.as_deref().unwrap_or_default();
				if with_id {
					print!("{}: ", paint.cyan(&format!("{:03}", b.id)));
				}
				if with_values {
					println!(
						"{} : {}",
						paint.green(&format!("{:>width$}", keyword)),
						paint.blue(&b.url)
					);
				} else {
					println!("{}", paint.green(keyword));
				}
			}
			Ok(())
		}
		Some(Command::TopVisited {
			limit,
			offset,
			json,
		}) => {
			let domains = visits::top_visited_domains(conn, limit, offset)?;
			if json {
				println!("{}", serde_json::to_string_pretty(&domains)?);
			} else {
				println!("{}", paint.bold("Most-visited domains:"));
				if domains.is_empty() {
					println!("  {}", paint.dim("(none yet)"));
				}
				for d in &domains {
					println!(
						"  {} {} visits across {} bookmark{}",
						paint.cyan(&format!("{:<32}", d.domain)),
						paint.bold(&d.total_visits.to_string()),
						paint.bold(&d.bookmark_count.to_string()),
						if d.bookmark_count == 1 { "" } else { "s" }
					);
				}
			}
			Ok(())
		}
		Some(Command::NeverVisited {
			limit,
			offset,
			json,
		}) => {
			let bookmarks = visits::never_visited(conn, limit, offset)?;
			if json {
				println!("{}", serde_json::to_string_pretty(&bookmarks)?);
			} else {
				println!(
					"{}",
					paint.bold(&format!("Never-visited bookmarks ({}):", bookmarks.len()))
				);
				if bookmarks.is_empty() {
					println!("  {}", paint.dim("(all bookmarks have been visited!)"));
				}
				for b in &bookmarks {
					println!(
						"  {} {} <{}>  ({})",
						paint.cyan(&format!("#{:<4}", b.id)),
						paint.bold(&b.title),
						paint.blue(b.url.as_str()),
						paint.dim(&output::relative_time(&b.created_at))
					);
				}
			}
			Ok(())
		}
		Some(Command::OrphanTags {
			limit,
			offset,
			json,
		}) => {
			let tags = tags::orphan_tags(conn, limit, offset)?;
			if json {
				println!("{}", serde_json::to_string_pretty(&tags)?);
			} else {
				println!(
					"{}",
					paint.bold(&format!(
						"Orphan tags (applied to only 1 bookmark, {}):",
						tags.len()
					))
				);
				if tags.is_empty() {
					println!("  {}", paint.dim("(none)"));
				}
				for t in &tags {
					println!(
						"  {} → {} {}",
						paint.magenta(&format!("{:<20}", t.name)),
						paint.cyan(&format!("#{}", t.bookmark_id)),
						paint.bold(&t.bookmark_title)
					);
				}
			}
			Ok(())
		}
		Some(Command::Hygiene { json }) => {
			let h = db_stats::hygiene(conn)?;
			if json {
				println!("{}", serde_json::to_string_pretty(&h)?);
			} else {
				// Percentages guard against divide-by-zero when the library
				// is empty (h.total == 0 → 0.0%), and get color-coded by
				// severity: green at 0%, yellow under a quarter, red beyond.
				let pct = |n: i64| {
					let p = if h.total > 0 {
						n as f64 / h.total as f64 * 100.0
					} else {
						0.0
					};
					let s = format!("{p:.0}%");
					let colored = if p == 0.0 {
						paint.green(&s)
					} else if p < 25.0 {
						paint.yellow(&s)
					} else {
						paint.red(&s)
					};
					format!("({colored})")
				};
				println!(
					"{}",
					paint.bold(&format!("Bookmark hygiene ({} total active):", h.total))
				);
				println!(
					"  Missing tags:         {} {}",
					paint.bold(&format!("{:<6}", h.missing_tags)),
					pct(h.missing_tags)
				);
				println!(
					"  Missing note:         {} {}",
					paint.bold(&format!("{:<6}", h.missing_note)),
					pct(h.missing_note)
				);
				println!(
					"  Missing description:  {} {}",
					paint.bold(&format!("{:<6}", h.missing_description)),
					pct(h.missing_description)
				);
			}
			Ok(())
		}
		Some(Command::Activity {
			limit,
			offset,
			json,
		}) => {
			let months = db_stats::monthly_activity(conn, limit, offset)?;
			if json {
				println!("{}", serde_json::to_string_pretty(&months)?);
			} else {
				println!("{}", paint.bold("Bookmarks added per month (last 12):"));
				if months.is_empty() {
					println!("  {}", paint.dim("(none yet)"));
				}
				for m in &months {
					println!(
						"  {}  {}",
						paint.cyan(&m.month),
						paint.bold(&format!("{:>4}", m.count))
					);
				}
			}
			Ok(())
		}
	}
}

fn print_overview(conn: &Connection, json: bool) -> Result<()> {
	let overview = db_stats::overview(conn)?;
	if json {
		println!("{}", serde_json::to_string_pretty(&overview)?);
	} else {
		output::print_stats_overview(&overview);
	}
	Ok(())
}
