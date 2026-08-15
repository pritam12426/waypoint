/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Netscape/HTML import and Markdown/CSV export — the interchange layer
//! behind the HTTP API (`POST /api/import`, `GET /api/export`).
//!
//! # Export
//!
//! `export_markdown` renders a `# Bookmarks` document grouped into `##`
//! headings per category, each bookmark as `- [title](url)` with an inline
//! keyword backtick and `#tag` hashtags, plus the description indented on
//! the next line. `export_csv` writes a flat RFC 4180-ish table. Both
//! return the rendered text as a `String` — the HTTP layer decides how to
//! hand it to the client.
//!
//! # Import
//!
//! `import_html` parses a Netscape bookmark file (every major browser's
//! export format). `<H3>` folder headings become categories; each `<A
//! HREF="...">Title</A>` becomes a bookmark tagged with the folder it
//! appeared under. The parse is regex-based on purpose: browser exports are
//! a small, consistent subset of HTML, and a full parser is more machinery
//! than that format needs. The caller supplies the file's *contents* (the
//! HTTP handler accepts them in the request body) rather than a path.
//!
//! Everything funnels through `database::bookmarks::insert`, so imports get
//! the same duplicate-URL protection and media resolution as interactive
//! adds — an already-present URL is skipped, not double-bookmarked.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::BTreeMap;
use utoipa::ToSchema;

use crate::database::bookmarks;
use crate::model::{Bookmark, DEFAULT_CATEGORY, NewBookmark};

/// The outcome of one import run: how many bookmarks were created and how
/// many were skipped (duplicate URLs and malformed entries).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
	pub imported: usize,
	pub skipped: usize,
}

// ============================================================
// Export
// ============================================================

/// Renders every active bookmark as a Markdown document, grouped by
/// category (`BTreeMap` = alphabetical category order). Only *active*
/// bookmarks are exported — trashed and archived content stays out of
/// backups by default.
pub fn export_markdown(conn: &Connection) -> Result<String> {
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

	crate::log_info!("exported {} bookmarks (markdown)", all.len());
	Ok(out)
}

/// Renders every active bookmark as a flat CSV table. Fields that can
/// contain the delimiter (title, url, description, tags, note, ...) are
/// RFC 4180-quoted via `csv_field`.
pub fn export_csv(conn: &Connection) -> Result<String> {
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

	crate::log_info!("exported {} bookmarks (csv)", all.len());
	Ok(out)
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

/// Imports a Netscape bookmark file's contents (the format every major
/// browser exports to). `<H3>` folder headings become categories; each `<A
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
///
/// The optional parameters override the per-file structure:
/// * `category` — when given, every imported bookmark is placed in that
///   category (created if missing) and the `<H3>` folder headings are
///   ignored for categorization. When `None`, folders still map to
///   categories and links outside any folder fall back to the default
///   category (`DEFAULT_CATEGORY`).
/// * `tags` — when given, these tags are added to every imported bookmark
///   (the file itself carries no tags).
/// * `archive` — when `true`, imported bookmarks are created directly in
///   the archive (the FTS `bookmarks_fts_archived` index) instead of the
///   active list.
pub fn import_html(
	conn: &Connection,
	content: &str,
	tags: Option<Vec<String>>,
	category: Option<String>,
	archive: bool,
) -> Result<ImportResult> {
	crate::log_debug!("import: parsing {} bytes of netscape html", content.len());

	// A category override overrides every `<H3>` folder; a blank value
	// counts as "not given" and keeps the folder-derived behavior.
	let category_override = category
		.map(|c| c.trim().to_string())
		.filter(|c| !c.is_empty());

	// One combined regex: an `<H3>...</H3>` capture (folder heading) OR an
	// `<A ...attributes...>Title</A>` capture. `(?is)` = dot-all +
	// case-insensitive, so attributes can span lines and tag names can be
	// any case (some exporters emit `href`, some `HREF`).
	let entry_re = regex::Regex::new(
		r#"(?is)<H3[^>]*>(?P<folder>.*?)</H3>|<A\s+(?P<attrs>[^>]*)>(?P<title>.*?)</A>"#,
	)?;
	// Pull the URL out of the attributes blob.
	let href_re = regex::Regex::new(r#"(?is)HREF\s*=\s*"([^"]*)""#)?;

	// Links outside any `<H3>` folder fall back to the default category.
	let mut current_category = DEFAULT_CATEGORY.to_string();
	let mut imported = 0;
	let mut skipped = 0;

	for cap in entry_re.captures_iter(content) {
		// Folder heading → switch the current category (unless a `category`
		// override pins every imported bookmark to one category).
		if let Some(folder) = cap.name("folder") {
			let name = html_unescape(folder.as_str().trim());
			if category_override.is_none() && !name.is_empty() {
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
			category: Some(
				category_override
					.clone()
					.unwrap_or_else(|| current_category.clone()),
			),
			tags: tags.clone(),
			keyword: None,
			redirect_template: None,
			note: None,
			favicon: None,
			thumbnail: None,
			favicon_mode: None,
			thumbnail_mode: None,
			starred: Some(false),
			is_archived: Some(archive),
		};

		match bookmarks::insert(conn, &new) {
			Ok(_) => imported += 1,
			Err(e) => {
				crate::log_warn!("skipped {url}: {e}");
				skipped += 1;
			}
		}
	}

	crate::log_info!("imported {imported} bookmarks ({skipped} skipped) (netscape)");
	Ok(ImportResult { imported, skipped })
}

/// Decodes the small set of HTML entities browsers actually emit in
/// bookmark exports. Order matters: `&amp;` must be last so it isn't
/// double-decoded (e.g. `&amp;lt;` → `&lt;`, not `<`).
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::database;
	use crate::model::BookmarkFilter;

	/// A minimal Netscape export: one link outside any folder (before the
	/// first `<H3>` heading), then an `<H3>` folder with a link inside it.
	const NETSCAPE: &str = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<DL><p>
<DT><A HREF="https://standalone.example/page">Standalone</A>
<DT><H3>Work</H3>
<DL><p>
<DT><A HREF="https://work.example/proj">Project</A>
</DL><p>
</DL><p>"#;

	fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
		let dir = tempfile::tempdir().expect("tempdir");
		let path = dir.path().join("waypointd_test.sqlite");
		(dir, path)
	}

	#[test]
	fn import_maps_folders_and_defaults_links_outside_them() {
		let (_dir, db_path) = temp_db();
		let conn = database::open(&db_path).unwrap();

		let result = import_html(&conn, NETSCAPE, None, None, false).unwrap();
		assert_eq!(result.imported, 2);
		assert_eq!(result.skipped, 0);

		let all = bookmarks::list(
			&conn,
			&BookmarkFilter {
				archived: Some(false),
				..Default::default()
			},
		)
		.unwrap();
		assert_eq!(all.len(), 2, "both links imported");

		let project = all
			.iter()
			.find(|b| b.url == "https://work.example/proj")
			.unwrap();
		assert_eq!(project.category_name.as_deref(), Some("Work"));
		assert!(project.tags.is_empty(), "no tag passed means no tags");
		assert!(!project.is_archived);

		let standalone = all
			.iter()
			.find(|b| b.url == "https://standalone.example/page")
			.unwrap();
		assert_eq!(
			standalone.category_name.as_deref(),
			Some(DEFAULT_CATEGORY),
			"links outside any folder fall back to the default category"
		);
	}

	#[test]
	fn import_applies_tags_category_and_archive_flags() {
		let (_dir, db_path) = temp_db();
		let conn = database::open(&db_path).unwrap();

		let result = import_html(
			&conn,
			NETSCAPE,
			Some(vec!["rust".to_string(), "todo".to_string()]),
			Some("Read Later".to_string()),
			true,
		)
		.unwrap();
		assert_eq!(result.imported, 2);

		// Every imported bookmark is archived, so the active list is empty.
		let active = bookmarks::list(
			&conn,
			&BookmarkFilter {
				archived: Some(false),
				..Default::default()
			},
		)
		.unwrap();
		assert!(
			active.is_empty(),
			"all imports went straight to the archive"
		);

		let archived = bookmarks::list(
			&conn,
			&BookmarkFilter {
				archived: Some(true),
				..Default::default()
			},
		)
		.unwrap();
		assert_eq!(archived.len(), 2);
		for b in &archived {
			assert_eq!(
				b.category_name.as_deref(),
				Some("Read Later"),
				"category override overrides the folder-derived category"
			);
			assert_eq!(b.tags, vec!["rust".to_string(), "todo".to_string()]);
			assert!(b.is_archived);
		}
	}

	#[test]
	fn import_skips_duplicates() {
		let (_dir, db_path) = temp_db();
		let conn = database::open(&db_path).unwrap();

		import_html(&conn, NETSCAPE, None, None, false).unwrap();
		let result = import_html(&conn, NETSCAPE, None, None, false).unwrap();
		assert_eq!(result.imported, 0);
		assert_eq!(result.skipped, 2, "second import is a no-op");
	}

	#[test]
	fn export_round_trips_active_bookmarks() {
		let (_dir, db_path) = temp_db();
		let conn = database::open(&db_path).unwrap();
		import_html(&conn, NETSCAPE, None, None, false).unwrap();

		let md = export_markdown(&conn).unwrap();
		assert!(md.starts_with("# Bookmarks\n"));
		assert!(md.contains("https://work.example/proj"));

		let csv = export_csv(&conn).unwrap();
		assert!(csv.starts_with("id,title,url,description,"));
		assert!(csv.contains("https://standalone.example/page"));
	}
}
