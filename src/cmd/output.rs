/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Human-readable rendering for CLI commands: bookmark lines, stats
//! breakdowns, ANSI paint, and relative-time formatting. All printers no-op
//! their colors when stdout isn't a terminal so piped output stays plain.

use std::io::IsTerminal;

use anyhow::Result;

use crate::model::{Bookmark, DEFAULT_CATEGORY, StatsOverview};

/// Wraps text in ANSI escape codes, no-oping when stdout is not a terminal
/// so piped output stays plain. Mirrors the TTY detection in
/// `logging::log_init`.
pub struct Paint(bool);

impl Paint {
	pub fn new() -> Self {
		Paint(std::io::stdout().is_terminal())
	}

	// Wraps one ANSI SGR code around `s`. The format is `ESC [ <code> m`
	// then `ESC [ 0 m` to reset. When stdout isn't a terminal (`self.0` is
	// false) we return the text untouched — no escape codes in pipes/files.
	fn wrap(&self, code: &str, s: &str) -> String {
		if self.0 {
			format!("\x1b[{code}m{s}\x1b[0m")
		} else {
			s.to_string()
		}
	}

	// Convenience wrappers; the codes are SGR "Select Graphic Rendition"
	// values (1 = bold, 2 = dim, 31–36 = the classic foreground colors).
	pub fn bold(&self, s: &str) -> String {
		self.wrap("1", s)
	}
	pub fn dim(&self, s: &str) -> String {
		self.wrap("2", s)
	}
	pub fn red(&self, s: &str) -> String {
		self.wrap("31", s)
	}
	pub fn green(&self, s: &str) -> String {
		self.wrap("32", s)
	}
	pub fn yellow(&self, s: &str) -> String {
		self.wrap("33", s)
	}
	pub fn blue(&self, s: &str) -> String {
		self.wrap("34", s)
	}
	pub fn magenta(&self, s: &str) -> String {
		self.wrap("35", s)
	}
	pub fn cyan(&self, s: &str) -> String {
		self.wrap("36", s)
	}
}

/// Converts a stored datetime string (UTC, "YYYY-MM-DD HH:MM:SS") into a
/// human-readable relative time string ("2 hours ago", "3 days ago", etc.).
pub fn relative_time(dt_str: &str) -> String {
	use chrono::{DateTime, Duration, Utc};

	// SQLite's CURRENT_TIMESTAMP stores "YYYY-MM-DD HH:MM:SS" with no zone
	// suffix, so append "+0000" and parse it as UTC. An unparseable string
	// (a NULL, a future schema change) is returned verbatim rather than
	// panicking the whole command.
	let Ok(parsed) = DateTime::parse_from_str(&format!("{dt_str} +0000"), "%Y-%m-%d %H:%M:%S %z")
	else {
		return dt_str.to_string();
	};
	let parsed: DateTime<Utc> = parsed.with_timezone(&Utc);
	let now = Utc::now();
	let diff = now - parsed;

	// Bucket thresholds are coarse by design: under a minute, under an
	// hour, under a day, under ~a month, under a year, else years.
	if diff < Duration::seconds(60) {
		"just now".to_string()
	} else if diff < Duration::minutes(60) {
		let mins = diff.num_minutes();
		format!("{mins} minute{} ago", if mins == 1 { "" } else { "s" })
	} else if diff < Duration::hours(24) {
		let hrs = diff.num_hours();
		format!("{hrs} hour{} ago", if hrs == 1 { "" } else { "s" })
	} else if diff < Duration::days(30) {
		let days = diff.num_days();
		format!("{days} day{} ago", if days == 1 { "" } else { "s" })
	} else if diff < Duration::days(365) {
		let months = diff.num_days() / 30;
		format!("{months} month{} ago", if months == 1 { "" } else { "s" })
	} else {
		let years = diff.num_days() / 365;
		format!("{years} year{} ago", if years == 1 { "" } else { "s" })
	}
}

pub fn print_bookmarks(bookmarks: &[Bookmark], json: bool, trash: bool) -> Result<()> {
	if json {
		// JSON output bypasses Paint entirely — the data is machine-encoded
		// by serde, never terminal-rendered.
		println!("{}", serde_json::to_string_pretty(bookmarks)?);
		return Ok(());
	}
	// The empty-state message depends on which surface we're rendering: a
	// recycle-bin listing says so, a plain listing doesn't.
	if bookmarks.is_empty() {
		if trash {
			println!("recycle bin is empty");
		} else {
			println!("no bookmarks found");
		}
		return Ok(());
	}
	let paint = Paint::new();
	for b in bookmarks {
		print_bookmark(b, &paint, trash);
	}
	Ok(())
}

pub fn print_bookmark(b: &Bookmark, paint: &Paint, trash: bool) {
	// The star renders as "★" or a single space so starred and unstarred
	// rows stay column-aligned even with proportional terminals.
	let star = if b.starred { "★" } else { " " };
	// Keyword shows inline in green inside brackets; a keyword that exists
	// but is empty is treated as "no keyword".
	let kw = b
		.keyword
		.as_deref()
		.filter(|k| !k.is_empty())
		.map(|k| format!(" [{}]", paint.green(k)))
		.unwrap_or_default();
	let cat = b.category_name.as_deref().unwrap_or(DEFAULT_CATEGORY);
	let archived = if b.is_archived { " (archived)" } else { "" };
	let trash_marker = if trash { " (in trash)" } else { "" };

	println!(
		"{} {} {}  <{}>{}  ({}){}{}",
		paint.cyan(&format!("#{:<4}", b.id)),
		paint.yellow(star),
		paint.bold(&b.title),
		paint.blue(&b.url),
		kw,
		paint.yellow(cat),
		paint.red(archived),
		paint.red(trash_marker),
	);
	if !b.tags.is_empty() {
		let tags = b
			.tags
			.iter()
			.map(|t| paint.magenta(t))
			.collect::<Vec<_>>()
			.join(", ");
		println!("      tags: {tags}");
	}
	if let Some(desc) = &b.description
		&& !desc.is_empty()
	{
		println!("      {}", paint.dim(desc));
	}
}

pub fn print_bookmark_detail(b: &Bookmark, paint: &Paint) {
	// The "detail" block used by `get`/`stats ids`: every field on its own
	// labeled line so scripts and humans can read it the same way. Optional
	// fields only print their line when present, keeping the block compact.
	println!(
		"{} {}",
		paint.cyan(&format!("#{}", b.id)),
		paint.bold(&b.title)
	);
	println!("  URL:          {}", b.url);
	if let Some(domain) = &b.domain {
		println!("  Domain:       {domain}");
	}
	if let Some(cat) = &b.category_name {
		println!("  Category:     {cat}");
	}
	if !b.tags.is_empty() {
		let tags = b
			.tags
			.iter()
			.map(|t| paint.magenta(t))
			.collect::<Vec<_>>()
			.join(", ");
		println!("  Tags:         {tags}");
	}
	println!("  Starred:      {}", if b.starred { "yes" } else { "no" });
	// The last-visit timestamp rides on the same line as the visit count so
	// the detail block doesn't grow a line for a rarely-set field.
	let last_visit = b
		.last_visited_at
		.as_deref()
		.map(|t| format!(" (last: {})", relative_time(t)))
		.unwrap_or_default();
	println!("  Visits:       {}{last_visit}", b.visit_count);
	println!(
		"  Created:      {} ({})",
		b.created_at,
		relative_time(&b.created_at)
	);
	println!(
		"  Updated:      {} ({})",
		b.updated_at,
		relative_time(&b.updated_at)
	);
	if let Some(note) = &b.note
		&& !note.is_empty()
	{
		println!("  Note:         {note}");
	}
	println!();
}

pub fn print_stats_overview(o: &StatsOverview) {
	let paint = Paint::new();
	println!(
		"Bookmarks: {} total, {} starred, {} archived, {} trashed",
		paint.bold(&o.total.to_string()),
		paint.yellow(&o.starred.to_string()),
		paint.blue(&o.archived.to_string()),
		paint.red(&o.trashed.to_string())
	);

	println!("\n{}", paint.bold("By category:"));
	if o.categories.is_empty() {
		println!("  {}", paint.dim("(none yet)"));
	}
	for c in &o.categories {
		println!(
			"  {} {}",
			paint.yellow(&format!("{:<32}", c.name)),
			paint.bold(&c.count.to_string())
		);
	}

	println!("\n{}", paint.bold("Top domains:"));
	if o.top_domains.is_empty() {
		println!("  {}", paint.dim("(none yet)"));
	}
	for d in &o.top_domains {
		println!(
			"  {} {}",
			paint.cyan(&format!("{:<32}", d.domain)),
			paint.bold(&d.count.to_string())
		);
	}

	println!("\n{}", paint.bold("Top tags:"));
	if o.top_tags.is_empty() {
		println!("  {}", paint.dim("(none yet)"));
	}
	for t in &o.top_tags {
		println!(
			"  {} {}",
			paint.magenta(&format!("{:<20}", t.name)),
			paint.bold(&t.count.to_string())
		);
	}

	println!("\n{}", paint.bold("Most visited:"));
	if o.most_visited.is_empty() {
		println!("  {}", paint.dim("(none yet)"));
	}
	for b in &o.most_visited {
		let domain = b.domain.as_deref().unwrap_or("");
		let last = b
			.last_visited_at
			.as_deref()
			.map(|t| format!(", last: {}", paint.dim(&relative_time(t))))
			.unwrap_or_default();
		println!(
			"  {} {} ({})  {} visits{}",
			paint.cyan(&format!("#{:<4}", b.id)),
			paint.bold(&b.title),
			paint.dim(domain),
			paint.bold(&b.visit_count.to_string()),
			last
		);
	}

	println!("\n{}", paint.bold("Recently added:"));
	if o.recently_added.is_empty() {
		println!("  {}", paint.dim("(none yet)"));
	}
	for b in &o.recently_added {
		let domain = b.domain.as_deref().unwrap_or("");
		println!(
			"  {} {} ({})  {}",
			paint.cyan(&format!("#{:<4}", b.id)),
			paint.bold(&b.title),
			paint.dim(domain),
			paint.dim(&relative_time(&b.created_at))
		);
	}
}
