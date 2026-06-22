/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Pure data model — the shapes that cross layer boundaries.
//!
//! # Why a dedicated model module
//!
//! Every layer (database, core, http, and the JSON the frontend consumes)
//! trades these structs around. Keeping them here, free of SQL and I/O,
//! means:
//!
//! * the same `Bookmark` struct the HTTP layer serializes is the same one
//!   the database produces — shapes can't drift;
//! * the structs are `ToSchema`-annotated, so `utoipa` can generate the
//!   OpenAPI spec straight from the types the API actually uses;
//! * nothing in this file has a side effect, which makes the write models
//!   (`NewBookmark`, `UpdateBookmark`) safe to construct in tests.
//!
//! # The tri-state pattern
//!
//! `UpdateBookmark` (and `Option`-based write fields generally) encode three
//! meanings with `Option<T>`: `None` = "leave unchanged", `Some(x)` = "set
//! to x", and — for fields where clearing matters, like `keyword` —
//! `Some("")` = "clear this field". This is what lets a partial update be
//! partial: the caller describes *differences*, and `database::bookmarks::update`
//! applies exactly those.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Category assigned to bookmarks that don't name one. Lives in the schema
/// (seeded by the initial migration) and in the code paths that fall back
/// to it, so the string has one canonical spelling.
///
/// The single spelling matters: `database::categories` treats this name as
/// special (it can't be renamed or deleted), and `insert` falls back to it
/// when a bookmark names no category.
pub const DEFAULT_CATEGORY: &str = "Uncategorized";

/// Reserved favicon value meaning "render the app's bundled default icon"
/// (`/favicon.ico` served by waypoint itself), not any remote URL.
///
/// Stored in the `favicon` column like a real URL, but the leading NUL
/// makes it impossible for an actual URL to collide with it (URLs cannot
/// contain NUL). Chosen over a new column so the schema stays put and the
/// existing empty-string sentinel idiom (`""` = generic domain favicon)
/// simply gains one sibling.
pub const DEFAULT_FAVICON: &str = "\0default-favicon";

/// Reserved thumbnail value meaning "render the app's bundled placeholder
/// thumbnail" (`/thumb-default.svg`). Same rationale and representation as
/// `DEFAULT_FAVICON`.
pub const DEFAULT_THUMBNAIL: &str = "\0default-thumbnail";

/// How a bookmark's favicon/thumbnail is decided when none is given
/// explicitly as a URL. Deliberately a tiny enum so the HTTP and database
/// layers all speak the same vocabulary:
///
/// * `Auto` — derive it: rule table first, then the generic domain
///   fallback (`favicon`) or nothing (`thumbnail`). The default behavior.
/// * `Default` — use the bundled default asset (Fevicol icon / placeholder
///   thumbnail); the `DEFAULT_FAVICON` / `DEFAULT_THUMBNAIL` token is stored.
/// * `Fetch` — the server performs a best-effort network fetch at save
///   time (page → `<link rel=icon>` for favicon, `og:image` for
///   thumbnail) and stores the discovered absolute URL. Degrades to the
///   auto result on any fetch failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AssetMode {
	Auto,
	Default,
	Fetch,
}

impl std::fmt::Display for AssetMode {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		// Mirrors the serde rename so log lines read like the API
		// vocabulary ("auto", "default", "fetch").
		f.write_str(match self {
			AssetMode::Auto => "auto",
			AssetMode::Default => "default",
			AssetMode::Fetch => "fetch",
		})
	}
}

/// A bookmark as stored and returned to clients (HTTP API).
///
/// This is the "row + a few joins flattened" shape: `category_name` and
/// `tags` come from joins, everything else maps 1:1 onto columns of the
/// `bookmarks` table. `trashed_at` is `None` for an active bookmark and
/// `Some(timestamp)` for one sitting in the recycle bin.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Bookmark {
	pub id: i64,
	pub title: String,
	pub url: String,
	pub description: Option<String>,
	pub domain: Option<String>,
	pub category_id: i64,
	pub category_name: Option<String>,
	pub starred: bool,
	pub keyword: Option<String>,
	pub note: Option<String>,
	pub favicon: Option<String>,
	pub thumbnail: Option<String>,
	pub visit_count: i64,
	pub last_visited_at: Option<String>,
	pub is_archived: bool,
	pub created_at: String,
	pub updated_at: String,
	pub trashed_at: Option<String>,
	pub tags: Vec<String>,
}

/// Payload for creating a bookmark. Every field but `url` is optional:
/// `title` defaults to the URL, `category` defaults to "Uncategorized".
///
/// `keyword`: an empty string and a missing/`null` value are both treated
/// as "no keyword" — see `database::bookmarks::insert`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewBookmark {
	pub url: String,
	pub title: Option<String>,
	pub description: Option<String>,
	pub category: Option<String>,
	pub tags: Option<Vec<String>>,
	pub keyword: Option<String>,
	pub note: Option<String>,
	/// Favicon URL, or `""` to force the generic domain `favicon.ico`
	/// (skipping site-specific/custom favicon rules). `None` auto-resolves.
	pub favicon: Option<String>,
	/// Thumbnail URL, or `""` to explicitly store none. `None` auto-resolves.
	pub thumbnail: Option<String>,
	/// How to resolve the favicon when no explicit `favicon` URL is given.
	/// When set, wins over `favicon`; `Auto` re-derives, `Default` stores
	/// the bundled-asset token, `Fetch` has the server fetch the page's
	/// icon at save time.
	pub favicon_mode: Option<AssetMode>,
	/// Same as `favicon_mode`, for the thumbnail.
	pub thumbnail_mode: Option<AssetMode>,
	pub starred: Option<bool>,
	/// Create the bookmark straight into the archive instead of the active
	/// list. `#[serde(default)]` keeps older JSON clients (which never send
	/// the field) creating active bookmarks.
	#[serde(default)]
	pub is_archived: Option<bool>,
}

/// Payload for updating a bookmark. This is a *partial* update: a field
/// left as `None` (omitted, or sent as JSON `null`) leaves that field
/// unchanged. `tags`, when present, fully replaces the tag set (an empty
/// array clears all tags). `keyword` has one extra state: `Some("")`
/// clears the keyword, `Some("x")` sets it, `None` leaves it alone.
///
/// `add_tags` / `remove_tags` are the incremental alternatives to the
/// destructive `tags` full-replace, so a client can say "add X" without
/// first fetching the whole current set.
#[derive(Debug, Clone, Deserialize, Default, ToSchema)]
pub struct UpdateBookmark {
	pub title: Option<String>,
	pub url: Option<String>,
	pub description: Option<String>,
	pub category: Option<String>,
	/// Full replacement: replaces the entire tag set (empty array clears all).
	pub tags: Option<Vec<String>>,
	/// Additive: adds these tags without touching the existing set.
	pub add_tags: Option<Vec<String>>,
	/// Subtractive: removes these tags from the existing set.
	pub remove_tags: Option<Vec<String>>,
	pub keyword: Option<String>,
	pub note: Option<String>,
	/// Favicon URL to set, `""` to reset to the generic domain `favicon.ico`
	/// (skipping site-specific rules), or `None` to leave unchanged.
	pub favicon: Option<String>,
	/// Thumbnail URL to set, `""` to clear, or `None` to leave unchanged.
	pub thumbnail: Option<String>,
	/// Favicon resolution mode: `Auto` re-derives from the (current) URL,
	/// `Default` resets to the bundled-asset token, `Fetch` re-fetches the
	/// page's icon now. Overrides `favicon` when set.
	pub favicon_mode: Option<AssetMode>,
	/// Same as `favicon_mode`, for the thumbnail.
	pub thumbnail_mode: Option<AssetMode>,
	/// Re-fetches the favicon and thumbnail from the page now, bypassing the
	/// fetched-media cache (90-day TTL). Explicit `favicon` / `thumbnail` /
	/// `*_mode` values in the same request still win.
	#[serde(default)]
	pub refresh: bool,
	pub starred: Option<bool>,
	pub is_archived: Option<bool>,
}

impl UpdateBookmark {
	/// Describes which fields this partial update actually changes relative
	/// to `existing`, for logging — so a log line says "starred #1" instead
	/// of a bare "updated #1". Mirrors the change detection in
	/// `database::bookmarks::update` (empty keyword clears, empty title is
	/// ignored, `tags` replaces the set).
	///
	/// The returned strings are human-readable verbs ("renamed",
	/// "set keyword to \"x\"", ...) that get joined into an audit message.
	pub fn describe(&self, existing: &Bookmark) -> Vec<String> {
		let mut ops = Vec::new();

		// Title: a blank title is ignored (never a deliberate "clear"),
		// and a same-as-before value is not a change worth logging.
		if self
			.title
			.as_deref()
			.is_some_and(|t| !t.trim().is_empty() && t != existing.title)
		{
			ops.push("renamed".to_string());
		}
		if self.url.as_deref().is_some_and(|u| u != existing.url) {
			ops.push("changed url".to_string());
		}
		// Keyword is the one genuinely tri-state field: `Some("")` clears.
		match self.keyword.as_deref() {
			Some("") => ops.push("cleared keyword".to_string()),
			Some(k) => ops.push(format!("set keyword to \"{k}\"")),
			None => {}
		}
		// Category is looked up by name downstream; a blank one is ignored.
		if let Some(c) = self.category.as_deref()
			&& !c.trim().is_empty()
		{
			ops.push(format!("moved to category \"{c}\""));
		}
		if self.tags.is_some() {
			ops.push("updated tags".to_string());
		}
		if let Some(add) = &self.add_tags
			&& !add.is_empty()
		{
			ops.push(format!("added tags {}", add.join(", ")));
		}
		if let Some(rm) = &self.remove_tags
			&& !rm.is_empty()
		{
			ops.push(format!("removed tags {}", rm.join(", ")));
		}
		// Booleans are two-state from the caller's perspective, so both
		// directions get their own verb.
		match self.starred {
			Some(true) => ops.push("starred".to_string()),
			Some(false) => ops.push("unstarred".to_string()),
			None => {}
		}
		match self.is_archived {
			Some(true) => ops.push("archived".to_string()),
			Some(false) => ops.push("unarchived".to_string()),
			None => {}
		}
		// The free-text fields compare `Some(actual) != Some(new)`, i.e. a
		// `Some("")` *does* count as "cleared" for these.
		if self
			.description
			.as_ref()
			.is_some_and(|d| Some(d) != existing.description.as_ref())
		{
			ops.push("updated description".to_string());
		}
		if self
			.note
			.as_ref()
			.is_some_and(|n| Some(n) != existing.note.as_ref())
		{
			ops.push("updated note".to_string());
		}
		// Favicon/thumbnail follow the keyword's tri-state idiom: `Some("")`
		// means "reset favicon to the generic domain favicon" / "clear
		// thumbnail" (see `database::bookmarks::update`), so each gets its
		// own verb instead of the generic "updated". The asset modes are
		// distinct instructions and describe themselves the same way.
		match self.favicon_mode {
			Some(AssetMode::Default) => ops.push("reset favicon to the bundled default".into()),
			Some(AssetMode::Fetch) => ops.push("fetched favicon from the page".into()),
			Some(AssetMode::Auto) => ops.push("re-derived favicon".into()),
			None => match self.favicon.as_deref() {
				Some("") => ops.push("reset favicon to default".to_string()),
				Some(f) if Some(f) != existing.favicon.as_deref() => {
					ops.push("updated favicon".to_string())
				}
				_ => {}
			},
		}
		match self.thumbnail_mode {
			Some(AssetMode::Default) => ops.push("set thumbnail to the bundled default".into()),
			Some(AssetMode::Fetch) => ops.push("fetched thumbnail from the page".into()),
			Some(AssetMode::Auto) => ops.push("re-derived thumbnail".into()),
			None => match self.thumbnail.as_deref() {
				Some("") => ops.push("cleared thumbnail".to_string()),
				Some(t) if Some(t) != existing.thumbnail.as_deref() => {
					ops.push("updated thumbnail".to_string())
				}
				_ => {}
			},
		}
		if self.refresh {
			ops.push("refreshed media".to_string());
		}

		ops
	}

	/// Whether any field actually asks for a change. A request with nothing
	/// set is a no-op; single updates tolerate it, but a bulk update must
	/// reject it to avoid a misleading "updated N bookmark(s)".
	pub fn has_any_change(&self) -> bool {
		self.title.is_some()
			|| self.url.is_some()
			|| self.description.is_some()
			|| self.category.is_some()
			|| self.tags.is_some()
			|| self.add_tags.is_some()
			|| self.remove_tags.is_some()
			|| self.keyword.is_some()
			|| self.note.is_some()
			|| self.favicon.is_some()
			|| self.thumbnail.is_some()
			|| self.favicon_mode.is_some()
			|| self.thumbnail_mode.is_some()
			|| self.refresh
			|| self.starred.is_some()
			|| self.is_archived.is_some()
	}
}

/// A category (the `categories` table row: id + unique name).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Category {
	pub id: i64,
	pub name: String,
}

/// A tag's name plus how many bookmarks use it. Used by `/api/tags`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TagCount {
	pub name: String,
	pub count: i64,
}

/// A domain plus how many bookmarks share it. Used by the domain-stats
/// views (top domains by bookmark count).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct DomainCount {
	pub domain: String,
	pub count: i64,
}

/// A category's name plus its bookmark count, for the overview.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct CategoryCount {
	pub name: String,
	pub count: i64,
}

/// A bookmark plus visit-derived fields, used in the "most visited" and
/// "recently added" lists of the stats overview.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct BookmarkVisitStats {
	pub id: i64,
	pub title: String,
	pub url: String,
	pub domain: Option<String>,
	pub visit_count: i64,
	pub last_visited_at: Option<String>,
	pub created_at: String,
}

/// The aggregated `GET /api/stats` / `stats` overview payload.
