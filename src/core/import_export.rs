/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Netscape/HTML import and Markdown/CSV export — the interchange layer
//! shared by the CLI (`import` / `export` commands) and any future API
//! surface.
//!
//! # Export
//!
//! `export_markdown` renders a `# Bookmarks` document grouped into `##`
//! headings per category, each bookmark as `- [title](url)` with an inline
//! keyword backtick and `#tag` hashtags, plus the description indented on
//! the next line. `export_csv` writes a flat RFC 4180-ish table. Both write
//! to a file, or to stdout when the path is `-` (so `waypoint export -f csv -`
//! can pipe into other tools).
//!
//! # Import
//!
//! `import_html` parses a Netscape bookmark file (every major browser's
//! export format). `<H3>` folder headings become categories; each `<A
//! HREF="...">Title</A>` becomes a bookmark tagged with the folder it
//! appeared under. The parse is regex-based on purpose: browser exports are
//! a small, consistent subset of HTML, and a full parser is more machinery
//! than that format needs.
//!
//! Everything funnels through `database::bookmarks::insert`, so imports get
//! the same duplicate-URL protection and media resolution as interactive
//! adds — an already-present URL is skipped, not double-bookmarked.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::Path;

use crate::database::bookmarks;
use crate::model::{Bookmark, DEFAULT_CATEGORY, NewBookmark};

// ============================================================
// Output
// ============================================================

/// Writes export content to a file, or to stdout when the path is `-`
/// (so exports can be piped: `waypoint export -f csv -`).
///
/// The trailing-newline guarantee matters for shell ergonomics: a piped
/// export should end on a clean line without the caller adding one.
pub fn write_output(path: &Path, content: &str) -> Result<()> {
	if path.as_os_str() == "-" {
		print!("{content}");
		if !content.ends_with('\n') {
			println!();
		}
		Ok(())
	} else {
		std::fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
	}
}

/// Renders every active bookmark as a Markdown document, grouped by
/// category (`BTreeMap` = alphabetical category order). Only *active*
/// bookmarks are exported — trashed and archived content stays out of
/// backups by default.
pub fn export_markdown(conn: &Connection, path: &Path) -> Result<()> {
	let all = bookmarks::list_all_active(conn)?;
	crate::log_debug!("markdown export: {} bookmarks", all.len());

	// Group by category. `category_name` is a JOIN result and can be NULL
	// (a category deleted mid-flight), so fall back to the default.
	let mut by_category: BTreeMap<String, Vec<&Bookmark>> = BTreeMap::new();
	for b in &all {
		let category = b
			.category_name
			.clone()
			.unwrap_or_else(|| DEFAULT_CATEGORY.to_string());
		by_category.entry(category).or_default().push(b);
	}

	let mut out = String::from("# Bookmarks\n\n");
	for (category, items) in &by_category {
		out.push_str(&format!("## {category}\n\n"));
		for b in items {
			out.push_str(&format!("- [{}]({})", b.title, b.url));
			// A set keyword is rendered as an inline code token; the empty
			// keyword (model's "no keyword") is filtered out.
			if let Some(kw) = b.keyword.as_deref().filter(|k| !k.is_empty()) {
				out.push_str(&format!(" `{kw}`"));
			}
			if !b.tags.is_empty() {
				let tags = b
					.tags
					.iter()
					.map(|t| format!("#{t}"))
					.collect::<Vec<_>>()
					.join(" ");
				out.push_str(&format!(" — {tags}"));
			}
			out.push('\n');
			if let Some(desc) = b.description.as_deref().filter(|d| !d.is_empty()) {
				out.push_str(&format!("  {desc}\n"));
			}
		}
		out.push('\n');
	}

	write_output(path, &out)?;
	crate::log_info!(
		"exported {} bookmarks to {} (markdown)",
		all.len(),
		path.display()
	);
	if path.as_os_str() != "-" {
		println!("exported {} bookmarks to {}", all.len(), path.display());
	}
	Ok(())
}

/// Renders every active bookmark as a flat CSV table. Fields that can
/// contain the delimiter (title, url, description, tags, note, ...) are
/// RFC 4180-quoted via `csv_field`.
pub fn export_csv(conn: &Connection, path: &Path) -> Result<()> {
	let all = bookmarks::list_all_active(conn)?;
	crate::log_debug!("csv export: {} bookmarks", all.len());

	let mut out =
		String::from("id,title,url,description,category,tags,keyword,note,favicon,starred\n");
	for b in &all {
		out.push_str(&format!(
			"{},{},{},{},{},{},{},{},{},{}\n",
			b.id,
			csv_field(&b.title),
			csv_field(&b.url),
			csv_field(b.description.as_deref().unwrap_or_default()),
			csv_field(b.category_name.as_deref().unwrap_or_default()),
			csv_field(&b.tags.join(" ")),
			csv_field(b.keyword.as_deref().unwrap_or_default()),
			csv_field(b.note.as_deref().unwrap_or_default()),
			csv_field(b.favicon.as_deref().unwrap_or_default()),
			csv_field(if b.starred { "true" } else { "false" }),
		));
	}

	write_output(path, &out)?;
	crate::log_info!(
		"exported {} bookmarks to {} (csv)",
		all.len(),
		path.display()
	);
	if path.as_os_str() != "-" {
		println!("exported {} bookmarks to {}", all.len(), path.display());
	}
	Ok(())
}

/// Quotes a CSV field when it contains a comma, quote, or newline, doubling
/// any embedded quotes (RFC 4180).
fn csv_field(s: &str) -> String {
	if s.chars().any(|c| matches!(c, ',' | '"' | '\n' | '\r')) {
		format!("\"{}\"", s.replace('"', "\"\""))
	} else {
		s.to_string()
	}
}

// ============================================================
// Import
// ============================================================

/// Imports a Netscape bookmark file (the format every major browser
/// exports to). `<H3>` folder headings become categories; each `<A
/// HREF="...">Title</A>` becomes a bookmark tagged with the folder it
/// appeared under.
///
/// This is a regex-based scan rather than a full HTML parser: browser
/// bookmark exports are a small, consistent subset of HTML, and a real
/// parser is more machinery than that format needs.
///
/// Import semantics:
/// * folders (`<H3>`) update the *current* category for the links that
///   follow (nesting isn't tracked — browsers flatten exports anyway);
/// * links without an `HREF`, or with an empty URL, are skipped silently;
/// * a blank title falls back to the URL;
/// * duplicates (URL already in the database) are skipped with a warning
///   rather than an error, so a second import of the same file is a no-op.
pub fn import_html(conn: &Connection, path: &Path) -> Result<()> {
	let content = std::fs::read_to_string(path)
		.with_context(|| format!("failed to read {}", path.display()))?;
	crate::log_debug!(
		"import: reading {} ({} bytes)",
		path.display(),
		content.len()
	);

	// One combined regex: an `<H3>...</H3>` capture (folder heading) OR an
	// `<A ...attributes...>Title</A>` capture. `(?is)` = dot-all +
	// case-insensitive, so attributes can span lines and tag names can be
	// any case (some exporters emit `href`, some `HREF`).
	let entry_re = regex::Regex::new(
		r#"(?is)<H3[^>]*>(?P<folder>.*?)</H3>|<A\s+(?P<attrs>[^>]*)>(?P<title>.*?)</A>"#,
	)?;
	// Pull the URL out of the attributes blob.
	let href_re = regex::Regex::new(r#"(?is)HREF\s*=\s*"([^"]*)""#)?;

	let mut current_category = "Imported".to_string();
	let mut imported = 0;
	let mut skipped = 0;

	for cap in entry_re.captures_iter(&content) {
		// Folder heading → switch the current category.
		if let Some(folder) = cap.name("folder") {
			let name = html_unescape(folder.as_str().trim());
			if !name.is_empty() {
				current_category = name;
			}
			continue;
		}

		// Otherwise an `<A>` entry: need both its attributes and title.
		let (Some(attrs), Some(title)) = (cap.name("attrs"), cap.name("title")) else {
			continue;
		};
		let Some(href_cap) = href_re.captures(attrs.as_str()) else {
			continue;
		};
		let url = html_unescape(&href_cap[1]);
		if url.is_empty() {
			continue;
		}
		let title = html_unescape(title.as_str().trim());

		let new = NewBookmark {
			title: Some(if title.is_empty() { url.clone() } else { title }),
			url: url.clone(),
			description: None,
			category: Some(current_category.clone()),
			tags: None,
			keyword: None,
			note: None,
			favicon: None,
			thumbnail: None,
			favicon_mode: None,
			thumbnail_mode: None,
			starred: Some(false),
		};

		match bookmarks::insert(conn, &new) {
			Ok(_) => imported += 1,
			Err(e) => {
				crate::log_warn!("skipped {url}: {e}");
				eprintln!("skipped {url}: {e}");
				skipped += 1;
			}
		}
	}

	crate::log_info!(
		"imported {imported} bookmarks ({skipped} skipped) from {} (netscape)",
		path.display()
	);
	println!("imported {imported} bookmarks ({skipped} skipped)");
	Ok(())
}

/// Decodes the small set of HTML entities browsers actually emit in
/// Decodes the handful of named HTML entities that can appear in bookmark
/// exports. Order matters: `&amp;` must be last so it isn't double-decoded
/// (e.g. `&amp;lt;` → `&lt;`, not `<`).
///
/// `pub(crate)` because `core::fetch` needs the exact same decoding for
/// URLs scraped out of `link`/`meta` attributes.
pub(crate) fn html_unescape(s: &str) -> String {
	s.replace("&lt;", "<")
		.replace("&gt;", ">")
		.replace("&quot;", "\"")
		.replace("&#39;", "'")
		.replace("&amp;", "&")
}
