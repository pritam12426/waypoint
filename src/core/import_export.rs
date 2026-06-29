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
