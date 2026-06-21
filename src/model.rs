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

