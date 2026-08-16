/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! In-crate tests for the persistence layer: schema init, idempotent reopen,
//! and the legacy-database upgrade path.
//!
//! The legacy test is the important one — it rebuilds a *pre-versioned*
//! database from the current schema (old column name, no FTS triggers, a
//! trashed row leaking into the main index) and proves `database::open`
//! repairs all of it. That's the safety net for real user data carried over
//! from the old builds.

use rusqlite::Connection;

use super::*;

/// The database layer logs through the crate logger, which drops messages
/// below its configured level. Initializing once at `Off` keeps test
/// output clean (unless `WAYPOINTD_LOG_LEVEL` is explicitly set in the
/// environment).
static LOG_INIT: std::sync::Once = std::sync::Once::new();
fn silence_logs() {
	LOG_INIT.call_once(|| {
		crate::logging::log_init(
			None,
			crate::logging::LogLevel::Off,
			crate::logging::LogFormat::Pretty,
		);
	});
}

/// Returns a live tempdir plus a database path inside it. The `TempDir`
/// must stay alive for the whole test, so it is returned alongside the path
/// (dropping it deletes the directory out from under SQLite).
fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
	let dir = tempfile::tempdir().expect("tempdir");
	let path = dir.path().join("waypoint_test.sqlite");
	(dir, path)
}

/// Every table SQLite reports for the schema, as a sorted list — used to
/// assert the fresh schema contains the expected tables.
fn table_names(conn: &Connection) -> Vec<String> {
	let mut stmt = conn
		.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
		.unwrap();
	stmt.query_map([], |row| row.get::<_, String>(0))
		.unwrap()
		.collect::<rusqlite::Result<Vec<_>>>()
		.unwrap()
}

/// A brand-new database comes out fully initialized: all expected tables and
/// the default category seeded.
#[test]
fn fresh_database_is_fully_initialized() {
	silence_logs();
	let (_dir, path) = temp_db();
	{
		let conn = open(&path).expect("open fresh database");
		let tables = table_names(&conn);
		for expected in [
			"bookmarks",
			"bookmarks_fts",
			"bookmarks_fts_archived",
			"bookmark_tags",
			"categories",
			"tags",
		] {
			assert!(
				tables.iter().any(|t| t == expected),
				"missing table {expected}"
			);
		}
		// Default category is seeded.
		let default: String = conn
			.query_row(
				"SELECT name FROM categories WHERE name = ?1",
				rusqlite::params![crate::model::DEFAULT_CATEGORY],
				|row| row.get(0),
			)
			.unwrap();
		assert_eq!(default, "Uncategorized");
	}
}

/// Reopening the same database is a no-op: the idempotent schema init
/// re-runs without error and the default-category seeding doesn't duplicate
/// rows.
#[test]
fn reopen_is_idempotent() {
	silence_logs();
	let (_dir, path) = temp_db();
	open(&path).unwrap();
	{
		let conn = open(&path).unwrap();
		// Still exactly one category row — seeding did not duplicate it.
		let count: i64 = conn
			.query_row("SELECT COUNT(*) FROM categories", [], |row| row.get(0))
			.unwrap();
		assert_eq!(count, 1);
	}
}

/// The full legacy-upgrade journey: a degraded pre-versioned database is
/// repaired by `open` — column renamed, triggers restored, index scrubbed.
#[test]
fn legacy_database_is_upgraded() {
	silence_logs();
	let (_dir, path) = temp_db();
	// Build a "legacy" database: start from the current schema, then degrade
	// it to look like a pre-versioned waypoint database (old column name,
	// no FTS triggers, no redirect-template column).
	{
		let mut conn = Connection::open(&path).unwrap();
		migrations::init(&mut conn).unwrap();
		// Insert bookmarks BEFORE degrading, while the triggers are still
		// live: a legacy database indexed every row (trashed ones included)
		// in the main index — that corruption is exactly what open() must
		// repair. Inserting after dropping the triggers would leave the
		// index empty, which is a different (never-real) state.
		conn.execute_batch(
			"INSERT INTO categories (name) VALUES ('Uncategorized');
			 INSERT INTO bookmarks (title, url, domain, category_id) VALUES ('legacy', 'https://a.example', 'a.example', 1);
			 INSERT INTO bookmarks (title, url, domain, category_id, trashed_at) VALUES ('gone', 'https://b.example', 'b.example', 1, CURRENT_TIMESTAMP);",
		)
		.unwrap();
		// Drop FTS triggers (required before the column rename, same reason
		// as in legacy_preclean).
		for trigger in super::LEGACY_FTS_TRIGGERS {
			conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger};"))
				.unwrap();
		}
		conn.execute_batch("ALTER TABLE bookmarks RENAME COLUMN trashed_at TO deleted_at;")
			.unwrap();
		// The redirect-template column postdates the legacy era — a
		// pre-versioned database never had it, so drop it to let the init
		// script's Rust-side guard add it back.
		conn.execute_batch("ALTER TABLE bookmarks DROP COLUMN redirect_template;")
			.unwrap();
	}

	// Reopening through the public entry point repairs everything.
	let conn = open(&path).expect("open legacy database");

	// Column renamed back (and the redirect-template column re-added).
	let cols: Vec<String> = {
		let mut stmt = conn
			.prepare("SELECT name FROM pragma_table_info('bookmarks')")
			.unwrap();
		stmt.query_map([], |row| row.get::<_, String>(0))
			.unwrap()
			.collect::<rusqlite::Result<Vec<_>>>()
			.unwrap()
	};
	assert!(
		cols.iter().any(|c| c == "trashed_at"),
		"trashed_at missing: {cols:?}"
	);
	assert!(
		!cols.iter().any(|c| c == "deleted_at"),
		"deleted_at still present: {cols:?}"
	);
	assert!(
		cols.iter().any(|c| c == "redirect_template"),
		"redirect_template missing: {cols:?}"
	);

	// The guarded FTS trigger set is back.
	let triggers: i64 = conn
		.query_row(
			"SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'bookmarks_fts%'",
			[],
			|row| row.get(0),
		)
		.unwrap();
	assert_eq!(triggers, 12);

	// Search index reflects reality: the trashed row is quarantined from
	// the main index.
	let active_hits: i64 = conn
		.query_row(
			"SELECT COUNT(*) FROM bookmarks_fts WHERE bookmarks_fts MATCH 'legacy'",
			[],
			|row| row.get(0),
		)
		.unwrap();
	assert_eq!(active_hits, 1);
	let trashed_hits: i64 = conn
		.query_row(
			"SELECT COUNT(*) FROM bookmarks_fts WHERE bookmarks_fts MATCH 'gone'",
			[],
			|row| row.get(0),
		)
		.unwrap();
	assert_eq!(trashed_hits, 0);
}

// ============================================================
// Media auto-resolution (favicon / thumbnail from the URL)
// ============================================================

use crate::database::bookmarks as bm_db;
use crate::model::{NewBookmark, UpdateBookmark};

/// Minimal bookmark with no media fields — the engine is free to derive.
fn plain_bookmark(url: &str) -> NewBookmark {
	NewBookmark {
		url: url.to_string(),
		title: None,
		description: None,
		category: None,
		tags: None,
		keyword: None,
		redirect_template: None,
		note: None,
		favicon: None,
		thumbnail: None,
		favicon_mode: None,
		thumbnail_mode: None,
		starred: None,
		is_archived: None,
	}
}

/// A new bookmark is never stored with a missing favicon: the media engine
/// derives it from the URL (Google's generic domain favicon fallback), and
/// sites with a thumbnail rule (YouTube) get a thumbnail too.
///
/// The YouTube leg is *cache-seeded*: the default resolution is cache-first
/// (the favicon column holds the channel avatar), so the test pre-populates
/// the media cache the way a prior successful fetch would, and asserts the
/// stored values are copied straight from it — no network involved.
#[test]
fn insert_populates_favicon_and_thumbnail_from_url() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	// Plain site: favicon from the domain fallback, no thumbnail rule.
	let id = bm_db::insert(&conn, &plain_bookmark("https://example.com/one")).unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://www.google.com/s2/favicons?sz=256&domain=example.com")
	);
	assert_eq!(b.thumbnail, None);

	// YouTube watch page: the cache-first resolution copies the cached
	// channel avatar into the favicon column and the cached CDN URL into
	// the thumbnail column.
	let yt_url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
	crate::core::cache::evict(yt_url);
	crate::core::cache::put(
		crate::core::media::MediaTarget::Favicon,
		yt_url,
		"https://yt3.googleusercontent.com/cached-channel-avatar",
	);
	crate::core::cache::put(
		crate::core::media::MediaTarget::Thumbnail,
		yt_url,
		"https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg",
	);
	let yt_id = bm_db::insert(&conn, &plain_bookmark(yt_url)).unwrap();
	let yt = bm_db::get(&conn, yt_id).unwrap().unwrap();
	assert_eq!(
		yt.favicon.as_deref(),
		Some("https://yt3.googleusercontent.com/cached-channel-avatar")
	);
	assert_eq!(
		yt.thumbnail.as_deref(),
		Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg")
	);
}

/// An explicit favicon/thumbnail in the payload is stored verbatim — the
/// auto-resolution only fills values the caller didn't provide.
#[test]
fn explicit_media_fields_are_kept() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let id = bm_db::insert(
		&conn,
		&NewBookmark {
			url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
			title: None,
			description: None,
			category: None,
			tags: None,
			keyword: None,
			redirect_template: None,
			note: None,
			favicon: Some("https://cdn.example/custom-icon.png".to_string()),
			thumbnail: Some("https://cdn.example/custom-thumb.png".to_string()),
			favicon_mode: None,
			thumbnail_mode: None,
			starred: None,
			is_archived: None,
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://cdn.example/custom-icon.png")
	);
	assert_eq!(
		b.thumbnail.as_deref(),
		Some("https://cdn.example/custom-thumb.png")
	);
}

/// Changing a bookmark's URL refreshes its favicon/thumbnail from the new
/// URL; changing to a URL with no thumbnail rule clears the stale one. An
/// explicit value in the same update still wins.
#[test]
fn url_change_refreshes_media() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let id = bm_db::insert(&conn, &plain_bookmark("https://example.com/one")).unwrap();
	assert_eq!(
		bm_db::get(&conn, id).unwrap().unwrap().favicon.as_deref(),
		Some("https://www.google.com/s2/favicons?sz=256&domain=example.com")
	);

	// The default resolution is cache-first for YouTube, so seed the media
	// cache the way a prior successful fetch would (no network in tests).
	let first_yt = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
	crate::core::cache::evict(first_yt);
	crate::core::cache::put(
		crate::core::media::MediaTarget::Favicon,
		first_yt,
		"https://yt3.googleusercontent.com/first-channel",
	);
	crate::core::cache::put(
		crate::core::media::MediaTarget::Thumbnail,
		first_yt,
		"https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg",
	);

	// URL change to YouTube → media follows the new URL (from the cache).
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			url: Some(first_yt.to_string()),
			..Default::default()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://yt3.googleusercontent.com/first-channel")
	);
	assert_eq!(
		b.thumbnail.as_deref(),
		Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg")
	);

	// URL change back to a rule-less site → thumbnail cleared, favicon
	// re-resolved to the new domain.
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			url: Some("https://other.example/page".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://www.google.com/s2/favicons?sz=256&domain=other.example")
	);
	assert_eq!(b.thumbnail, None);

	// An explicit favicon in the same URL-changing update wins over the
	// derivation; the thumbnail comes from the (seeded) cache.
	let second_yt = "https://www.youtube.com/watch?v=AbCdEf123";
	crate::core::cache::evict(second_yt);
	crate::core::cache::put(
		crate::core::media::MediaTarget::Thumbnail,
		second_yt,
		"https://i.ytimg.com/vi/AbCdEf123/hqdefault.jpg",
	);
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			url: Some(second_yt.to_string()),
			favicon: Some("https://cdn.example/keep-this.png".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://cdn.example/keep-this.png")
	);
	assert_eq!(
		b.thumbnail.as_deref(),
		Some("https://i.ytimg.com/vi/AbCdEf123/hqdefault.jpg")
	);
}

/// An update that doesn't touch the URL leaves the stored media alone.
#[test]
fn url_unchanged_keeps_media() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let id = bm_db::insert(&conn, &plain_bookmark("https://example.com/one")).unwrap();
	let favicon = bm_db::get(&conn, id).unwrap().unwrap().favicon.clone();

	// Title-only update: URL untouched, media must not be recomputed.
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			title: Some("Renamed".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(b.title, "Renamed");
	assert_eq!(b.favicon, favicon);
}

/// Resending the *same* URL value — exactly what the web UI's edit form does
/// on every save — must not re-derive media. Only an actual URL change may
/// recompute (and clobber) custom favicon/thumbnail URLs.
#[test]
fn url_resent_unchanged_keeps_custom_media() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let mut new = plain_bookmark("https://example.com/one");
	new.favicon = Some("https://cdn.example/custom-icon.png".to_string());
	new.thumbnail = Some("https://cdn.example/custom-thumb.png".to_string());
	let id = bm_db::insert(&conn, &new).unwrap();

	// The edit form resends the stored URL verbatim alongside a title change.
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			title: Some("Renamed".to_string()),
			url: Some("https://example.com/one".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(b.title, "Renamed");
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://cdn.example/custom-icon.png")
	);
	assert_eq!(
		b.thumbnail.as_deref(),
		Some("https://cdn.example/custom-thumb.png")
	);
}

/// `refresh` (the `--refresh` flag) re-fetches favicon/thumbnail even when
/// the URL is unchanged, bypassing the fetched-media cache. Here a cached
/// value exists: plain `Fetch` mode honors it, but `refresh: true` ignores
/// it and re-scrapes — the connection-refused fetch degrades to the domain
/// fallback, and since offline fallbacks are never cached the stale value
/// stays gone.
#[test]
fn refresh_bypasses_the_media_cache() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let url = "http://127.0.0.1:1/db-refresh";
	let id = bm_db::insert(&conn, &plain_bookmark(url)).unwrap();

	// Cache a fake "successful" fetch for this URL, then re-fetch in
	// `Fetch` mode: the cache is honored.
	crate::core::cache::put(
		crate::core::media::MediaTarget::Favicon,
		url,
		"https://cached.example/icon.png",
	);
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			favicon_mode: Some(AssetMode::Fetch),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(
		bm_db::get(&conn, id).unwrap().unwrap().favicon.as_deref(),
		Some("https://cached.example/icon.png")
	);

	// `refresh: true` skips the cache and re-scrapes now; the
	// connection-refused fetch degrades to the generic domain favicon.
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			refresh: true,
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(
		bm_db::get(&conn, id).unwrap().unwrap().favicon.as_deref(),
		Some("https://www.google.com/s2/favicons?sz=256&domain=127.0.0.1")
	);
}

/// The empty-string sentinel (`--no-custom-favicon` / `--no-thumbnail`):
/// insert with `favicon: Some("")` stores the generic domain favicon (the
/// rule table is skipped entirely), and `thumbnail: Some("")` stores none —
/// even for a YouTube URL that would otherwise derive a CDN thumbnail.
#[test]
fn insert_sentinel_forces_generic_media() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let id = bm_db::insert(
		&conn,
		&NewBookmark {
			url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
			title: None,
			description: None,
			category: None,
			tags: None,
			keyword: None,
			redirect_template: None,
			note: None,
			favicon: Some(String::new()),
			thumbnail: Some(String::new()),
			favicon_mode: None,
			thumbnail_mode: None,
			starred: None,
			is_archived: None,
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://www.google.com/s2/favicons?sz=256&domain=www.youtube.com")
	);
	assert_eq!(b.thumbnail, None);
}

/// `update` with the sentinel resets media regardless of URL change: the
/// favicon snaps to the generic domain favicon and the thumbnail clears.
/// An explicit URL in the same update still wins for the favicon's target
/// domain.
#[test]
fn update_sentinel_resets_media() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	// A bookmark with custom media from the "before" state.
	let id = bm_db::insert(
		&conn,
		&NewBookmark {
			url: "https://example.com/one".to_string(),
			title: None,
			description: None,
			category: None,
			tags: None,
			keyword: None,
			redirect_template: None,
			note: None,
			favicon: Some("https://cdn.example/custom-icon.png".to_string()),
			thumbnail: Some("https://cdn.example/custom-thumb.png".to_string()),
			favicon_mode: None,
			thumbnail_mode: None,
			starred: None,
			is_archived: None,
		},
	)
	.unwrap();

	// No URL change: reset favicon to the *current* domain's generic
	// favicon, clear the thumbnail.
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			favicon: Some(String::new()),
			thumbnail: Some(String::new()),
			..Default::default()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://www.google.com/s2/favicons?sz=256&domain=example.com")
	);
	assert_eq!(b.thumbnail, None);

	// With a URL change, the generic favicon follows the new domain.
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			url: Some("https://other.example/page".to_string()),
			favicon: Some(String::new()),
			..Default::default()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://www.google.com/s2/favicons?sz=256&domain=other.example")
	);
	assert_eq!(b.thumbnail, None);
}

// ============================================================
// Media modes (Auto / Default / Fetch) and criteria-based removal
// ============================================================

use crate::model::{AssetMode, BookmarkFilter, DEFAULT_FAVICON, DEFAULT_THUMBNAIL};

/// A bookmark with a category and tag set, plus a keyword.
fn tagged_bookmark(url: &str, category: &str, tag: &str, keyword: Option<&str>) -> NewBookmark {
	let mut b = plain_bookmark(url);
	b.category = Some(category.to_string());
	b.tags = Some(vec![tag.to_string()]);
	b.keyword = keyword.map(str::to_string);
	b
}

/// `Default` mode stores the bundled-asset tokens verbatim — the frontend
/// renders `/favicon.ico` / `/thumb-default.svg` for those, not a remote URL.
#[test]
fn insert_default_mode_stores_bundled_asset_tokens() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let id = bm_db::insert(
		&conn,
		&NewBookmark {
			url: "https://example.com/one".to_string(),
			favicon_mode: Some(AssetMode::Default),
			thumbnail_mode: Some(AssetMode::Default),
			..plain_bookmark_media_defaults()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(b.favicon.as_deref(), Some(DEFAULT_FAVICON));
	assert_eq!(b.thumbnail.as_deref(), Some(DEFAULT_THUMBNAIL));
}

/// An explicit payload that literally equals a bundled-default token is
/// refused: it would be stored verbatim and then render as the bundled
/// asset, not the URL the user meant to save.
#[test]
fn insert_rejects_payload_colliding_with_bundled_token() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let err = bm_db::insert(
		&conn,
		&NewBookmark {
			url: "https://example.com/one".to_string(),
			favicon: Some(DEFAULT_FAVICON.to_string()),
			..plain_bookmark_media_defaults()
		},
	)
	.unwrap_err();
	assert!(err.to_string().contains("bundled-default token"));

	let err = bm_db::insert(
		&conn,
		&NewBookmark {
			url: "https://example.com/two".to_string(),
			thumbnail: Some(DEFAULT_THUMBNAIL.to_string()),
			..plain_bookmark_media_defaults()
		},
	)
	.unwrap_err();
	assert!(err.to_string().contains("bundled-default token"));
}

/// `update --mode default` overrides a stored custom favicon: the token
/// replaces the URL, and `--mode auto` later re-derives from the URL.
#[test]
fn update_mode_default_replaces_custom_favicon() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let id = bm_db::insert(
		&conn,
		&NewBookmark {
			url: "https://example.com/one".to_string(),
			favicon: Some("https://cdn.example/custom.png".to_string()),
			..plain_bookmark_media_defaults()
		},
	)
	.unwrap();
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			favicon_mode: Some(AssetMode::Default),
			..Default::default()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(b.favicon.as_deref(), Some(DEFAULT_FAVICON));

	// Back to auto: the stored token is replaced by a URL-derived icon.
	bm_db::update(
		&conn,
		id,
		&UpdateBookmark {
			favicon_mode: Some(AssetMode::Auto),
			..Default::default()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://www.google.com/s2/favicons?sz=256&domain=example.com")
	);
}

/// `Fetch` mode degrades to the auto result when the network is unreachable
/// (connection-refused on `127.0.0.1:1` — no external network in tests).
#[test]
fn insert_fetch_mode_degrades_to_auto_on_failure() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let id = bm_db::insert(
		&conn,
		&NewBookmark {
			url: "http://127.0.0.1:1/fail".to_string(),
			favicon_mode: Some(AssetMode::Fetch),
			thumbnail_mode: Some(AssetMode::Fetch),
			..plain_bookmark_media_defaults()
		},
	)
	.unwrap();
	let b = bm_db::get(&conn, id).unwrap().unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://www.google.com/s2/favicons?sz=256&domain=127.0.0.1")
	);
	assert_eq!(b.thumbnail, None);
}

/// Criteria removal by tag and by category: only the matching bookmarks
/// move to the trash, and the returned ids match exactly.
#[test]
fn remove_matching_by_tag_and_category() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let a = bm_db::insert(
		&conn,
		&tagged_bookmark("https://a.example", "work", "rust", None),
	)
	.unwrap();
	let b = bm_db::insert(
		&conn,
		&tagged_bookmark("https://b.example", "work", "web", Some("bh")),
	)
	.unwrap();
	let c = bm_db::insert(
		&conn,
		&tagged_bookmark("https://c.example", "home", "rust", None),
	)
	.unwrap();

	// Tag filter hits bookmarks across categories.
	let result = bm_db::remove_matching(
		&conn,
		&BookmarkFilter {
			tag: Some("rust".to_string()),
			..Default::default()
		},
		false,
	)
	.unwrap();
	assert_eq!(result.removed, 2);
	assert_eq!(result.ids.len(), 2);
	assert!(result.ids.contains(&a));
	assert!(result.ids.contains(&c));
	for id in [a, c] {
		assert!(
			bm_db::get(&conn, id).unwrap().is_none(),
			"#{id} should be trashed"
		);
	}
	// "web" bookmark b is untouched.
	assert!(bm_db::get(&conn, b).unwrap().is_some());
	// Trashed bookmarks are no longer trashed-again eligible; a second run
	// matches nothing.
	let again = bm_db::remove_matching(
		&conn,
		&BookmarkFilter {
			tag: Some("rust".to_string()),
			..Default::default()
		},
		false,
	)
	.unwrap();
	assert_eq!(again.removed, 0);
	assert!(again.ids.is_empty());

	// Category filter on the remaining active one.
	let result = bm_db::remove_matching(
		&conn,
		&BookmarkFilter {
			category: Some("work".to_string()),
			..Default::default()
		},
		false,
	)
	.unwrap();
	assert_eq!(result.removed, 1);
	assert_eq!(result.ids, vec![b]);

	// Keyword criteria work the same way.
	let result = bm_db::remove_matching(
		&conn,
		&BookmarkFilter {
			keyword: Some("bh".to_string()),
			..Default::default()
		},
		false,
	)
	.unwrap();
	assert_eq!(result.removed, 0, "already trashed");
}

/// `remove_ids` skips stale ids and ids already in the trash; the returned
/// ids list is exactly what changed.
#[test]
fn remove_ids_skips_stale_and_trashed() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let a = bm_db::insert(&conn, &plain_bookmark("https://a.example")).unwrap();
	let b = bm_db::insert(&conn, &plain_bookmark("https://b.example")).unwrap();
	bm_db::trash(&conn, b).unwrap();

	let result = bm_db::remove_ids(&conn, &[a, b, 999], false).unwrap();
	assert_eq!(result.removed, 1, "only the active id is trashed");
	assert_eq!(result.ids, vec![a]);
}

/// Trash-empty semantics: `trash: true` + `trashed_before` scopes the purge
/// to bookmarks trashed on or before the bound, and purged rows are gone.
#[test]
fn trash_empty_honors_trashed_before_bound() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let a = bm_db::insert(&conn, &plain_bookmark("https://a.example")).unwrap();
	let b = bm_db::insert(&conn, &plain_bookmark("https://b.example")).unwrap();
	bm_db::trash(&conn, a).unwrap();
	bm_db::trash(&conn, b).unwrap();
	// Pin distinct trash timestamps so the bound distinguishes them.
	conn.execute(
		"UPDATE bookmarks SET trashed_at = '2024-01-01 00:00:00' WHERE id = ?1",
		rusqlite::params![a],
	)
	.unwrap();
	conn.execute(
		"UPDATE bookmarks SET trashed_at = '2024-06-01 00:00:00' WHERE id = ?1",
		rusqlite::params![b],
	)
	.unwrap();

	// Purging everything trashed before 2024-03-01 gets only the January one.
	let result = bm_db::remove_matching(
		&conn,
		&BookmarkFilter {
			trash: true,
			trashed_before: Some("2024-03-01 23:59:59".to_string()),
			..Default::default()
		},
		true,
	)
	.unwrap();
	assert_eq!(result.removed, 1);
	assert_eq!(result.ids, vec![a]);
	assert!(bm_db::get(&conn, a).unwrap().is_none());
	assert!(bm_db::get(&conn, b).unwrap().is_none(), "b still in trash");

	// The rest empties out now.
	let rest = bm_db::remove_matching(
		&conn,
		&BookmarkFilter {
			trash: true,
			..Default::default()
		},
		true,
	)
	.unwrap();
	assert_eq!(rest.removed, 1);
	assert_eq!(rest.ids, vec![b]);
}

/// `created_after`/`created_before` bounds are inclusive of whole days: a
/// bare date picks up everything created that day, start to finish.
#[test]
fn time_bounds_cover_whole_days_inclusive() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let a = bm_db::insert(&conn, &plain_bookmark("https://a.example")).unwrap();
	let b = bm_db::insert(&conn, &plain_bookmark("https://b.example")).unwrap();
	// Pin explicit timestamps for determinism.
	conn.execute(
		"UPDATE bookmarks SET created_at = '2024-01-01 00:00:00' WHERE id = ?1",
		rusqlite::params![a],
	)
	.unwrap();
	conn.execute(
		"UPDATE bookmarks SET created_at = '2024-06-01 12:00:00' WHERE id = ?1",
		rusqlite::params![b],
	)
	.unwrap();

	// The whole 2024-01-01 day is included by a bare-date after bound. The
	// database layer takes pre-normalized `YYYY-MM-DD HH:MM:SS` bounds (the
	// HTTP layer expands bare dates to 00:00:00 / 23:59:59 via
	// `shared::parse_datetime_bound`).
	let day = bm_db::list(
		&conn,
		&BookmarkFilter {
			created_after: Some("2024-01-01 00:00:00".to_string()),
			created_before: Some("2024-01-01 23:59:59".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(day.len(), 1);
	assert_eq!(day[0].id, a);

	let midyear = bm_db::list(
		&conn,
		&BookmarkFilter {
			created_after: Some("2024-05-01 00:00:00".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(midyear.len(), 1);
	assert_eq!(midyear[0].id, b);
}

/// Search narrowing by tag and category filters the FTS results and the
/// count agrees with the returned rows.
#[test]
fn search_narrows_by_tag_and_category() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let a = bm_db::insert(
		&conn,
		&tagged_bookmark("https://a.example/rust-notes", "work", "rust", None),
	)
	.unwrap();
	bm_db::insert(
		&conn,
		&tagged_bookmark("https://b.example/rust-guide", "home", "web", None),
	)
	.unwrap();

	let results = bm_db::search(
		&conn,
		"rust",
		20,
		&BookmarkFilter {
			category: Some("work".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(results.len(), 1);
	assert_eq!(results[0].id, a);
	assert_eq!(
		bm_db::count_search(
			&conn,
			"rust",
			false,
			&BookmarkFilter {
				category: Some("work".to_string()),
				..Default::default()
			}
		)
		.unwrap(),
		1
	);

	// Tag narrowing on the same query.
	let tagged = bm_db::search(
		&conn,
		"rust",
		20,
		&BookmarkFilter {
			tag: Some("rust".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(tagged.len(), 1);
}

/// Keywords are case-insensitive: a browser address bar is informal, so
/// `II`, `Ii`, and `ii` must all resolve to the same shortcut, and a
/// case-variant must not be creatable as a second bookmark.
#[test]
fn keyword_lookup_is_case_insensitive() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let mut b = tagged_bookmark("https://ii.example", "work", "rust", Some("ii"));
	b.favicon = None;
	b.thumbnail = None;
	let id = bm_db::insert(&conn, &b).unwrap();

	// The lookup folds case in both directions.
	for kw in ["II", "Ii", "ii"] {
		let found = bm_db::get_by_keyword(&conn, kw).unwrap();
		assert_eq!(found.map(|f| f.id), Some(id), "keyword {kw} must match");
	}

	// A case-variant is the same keyword: the friendly pre-check rejects it.
	let mut variant = tagged_bookmark("https://other.example", "work", "rust", Some("II"));
	variant.favicon = None;
	variant.thumbnail = None;
	let err = bm_db::insert(&conn, &variant).unwrap_err().to_string();
	assert!(
		err.contains("already in use"),
		"case-variant must conflict: {err}"
	);

	// The filter paths are case-insensitive too.
	let list = bm_db::list(
		&conn,
		&BookmarkFilter {
			keyword: Some("II".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(list.len(), 1);
}

/// A URL-changing `update` that collides with another *active* bookmark's
/// URL is rejected up front — before any media resolution — so it can't
/// trigger a needless network fetch or cache write for the colliding URL.
/// Re-sending the current URL (a no-op resend) must not trip it.
#[test]
fn update_rejects_colliding_url_before_media_resolution() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	// Insert A (YouTube, cache-seeded so no network), then B. The video id
	// must be unique to this test: tests share one per-process cache dir, so
	// a URL used by another test would let this one's `evict` race it.
	let a_url = "https://www.youtube.com/watch?v=UuNu1q3dQw4";
	crate::core::cache::evict(a_url);
	crate::core::cache::put(
		crate::core::media::MediaTarget::Favicon,
		a_url,
		"https://yt3.googleusercontent.com/avatar",
	);
	let a_id = bm_db::insert(&conn, &plain_bookmark(a_url)).unwrap();
	let b_id = bm_db::insert(&conn, &plain_bookmark("https://example.com/two")).unwrap();

	// Drop A's cached media as if it had expired: without the guard, the
	// colliding update would re-fetch it (network + cache write).
	crate::core::cache::evict(a_url);
	assert_eq!(
		crate::core::cache::get(crate::core::media::MediaTarget::Favicon, a_url),
		None,
		"precondition: A's media cache is empty"
	);

	// Changing B's URL to A's URL is a friendly error...
	let err = bm_db::update(
		&conn,
		b_id,
		&UpdateBookmark {
			url: Some(a_url.to_string()),
			..Default::default()
		},
	)
	.unwrap_err()
	.to_string();
	assert!(
		err.contains("URL already exists as bookmark") && err.contains(&format!("#{a_id}")),
		"expected a friendly duplicate error, got: {err}"
	);

	// ...and it must not have fetched/cached anything for the colliding URL,
	// nor touched B's row.
	assert_eq!(
		crate::core::cache::get(crate::core::media::MediaTarget::Favicon, a_url),
		None,
		"the colliding update must not populate the media cache"
	);
	let b = bm_db::get(&conn, b_id).unwrap().unwrap();
	assert_eq!(b.url, "https://example.com/two");

	// Re-sending the current URL is not a collision.
	bm_db::update(
		&conn,
		b_id,
		&UpdateBookmark {
			url: Some("https://example.com/two".to_string()),
			title: Some("renamed".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(bm_db::get(&conn, b_id).unwrap().unwrap().title, "renamed");
}

/// A keyword-changing `update` that collides with another *active* bookmark's
/// keyword is rejected up front — before any media resolution — so it can't
/// trigger a needless network fetch or cache write for the new URL. The
/// collision check is NOCASE (a case-variant is the same shortcut), and
/// re-saving / case-folding one's own keyword is not a collision.
#[test]
fn update_rejects_colliding_keyword_before_media_resolution() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	// A owns keyword "alpha". B is plain. The video id must be unique to
	// this test: tests share one per-process cache dir.
	let a_url = "https://www.youtube.com/watch?v=AaBb3cDdEe4";
	crate::core::cache::evict(a_url);
	crate::core::cache::put(
		crate::core::media::MediaTarget::Favicon,
		a_url,
		"https://yt3.googleusercontent.com/avatar",
	);
	let a_id = bm_db::insert(
		&conn,
		&tagged_bookmark(a_url, "work", "rust", Some("alpha")),
	)
	.unwrap();
	let b_url = "https://example.com/two";
	let b_id = bm_db::insert(&conn, &plain_bookmark(b_url)).unwrap();

	// A fresh URL whose cache is empty: if the keyword check didn't run
	// first, a URL-changing update would fetch media for it.
	let colliding_url = "https://www.youtube.com/watch?v=FfGg5hHhIi6";
	crate::core::cache::evict(colliding_url);
	assert_eq!(
		crate::core::cache::get(crate::core::media::MediaTarget::Favicon, colliding_url),
		None,
		"precondition: colliding URL's media cache is empty"
	);

	// Changing B's URL to a fresh one *and* grabbing A's keyword is a
	// friendly error...
	let err = bm_db::update(
		&conn,
		b_id,
		&UpdateBookmark {
			url: Some(colliding_url.to_string()),
			keyword: Some("alpha".to_string()),
			..Default::default()
		},
	)
	.unwrap_err()
	.to_string();
	assert!(
		err.contains("keyword \"alpha\" already in use") && err.contains(&format!("#{a_id}")),
		"expected a friendly keyword error, got: {err}"
	);

	// ...and it must not have fetched/cached anything for the colliding URL,
	// nor touched B's row.
	assert_eq!(
		crate::core::cache::get(crate::core::media::MediaTarget::Favicon, colliding_url),
		None,
		"the colliding update must not populate the media cache"
	);
	let b = bm_db::get(&conn, b_id).unwrap().unwrap();
	assert_eq!(b.url, b_url);
	assert_eq!(b.keyword, None);

	// A case-variant is the same keyword: also rejected (and before media).
	let err = bm_db::update(
		&conn,
		b_id,
		&UpdateBookmark {
			url: Some(colliding_url.to_string()),
			keyword: Some("ALPHA".to_string()),
			..Default::default()
		},
	)
	.unwrap_err()
	.to_string();
	assert!(
		err.contains("keyword \"ALPHA\" already in use"),
		"case-variant must conflict: {err}"
	);
	assert_eq!(
		crate::core::cache::get(crate::core::media::MediaTarget::Favicon, colliding_url),
		None,
		"the case-variant colliding update must not populate the media cache"
	);

	// Re-sending the current (absent) keyword is a no-op, not a collision —
	// clearing without a value must succeed.
	bm_db::update(
		&conn,
		b_id,
		&UpdateBookmark {
			keyword: Some(String::new()),
			title: Some("renamed".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(bm_db::get(&conn, b_id).unwrap().unwrap().title, "renamed");

	// Case-folding one's own keyword stays allowed: the collision check
	// excludes the row being updated.
	bm_db::update(
		&conn,
		b_id,
		&UpdateBookmark {
			keyword: Some("BETA".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	bm_db::update(
		&conn,
		b_id,
		&UpdateBookmark {
			keyword: Some("beta".to_string()),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(
		bm_db::get(&conn, b_id).unwrap().unwrap().keyword.as_deref(),
		Some("beta")
	);
}

/// `is_unique_violation` recognizes the raw UNIQUE-constraint error (extended
/// code 2067) that a race between the friendly pre-checks and a concurrent
/// writer surfaces at INSERT/UPDATE time. The `ffi::Error` fields are public,
/// so the matcher is testable directly.
#[test]
fn is_unique_violation_matches_constraint_unique() {
	let err = rusqlite::Error::SqliteFailure(
		rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE),
		None,
	);
	assert!(bm_db::is_unique_violation(&err));

	let other = rusqlite::Error::SqliteFailure(
		rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_NOTNULL),
		None,
	);
	assert!(!bm_db::is_unique_violation(&other));

	let not_sqlite = rusqlite::Error::InvalidParameterName("nope".into());
	assert!(!bm_db::is_unique_violation(&not_sqlite));
}

/// The bookmark used for `insert` in the mode tests: same shape as
/// `plain_bookmark` but expressed as a spread so the mode fields can be set.
fn plain_bookmark_media_defaults() -> NewBookmark {
	NewBookmark {
		url: String::new(),
		title: None,
		description: None,
		category: None,
		tags: None,
		keyword: None,
		redirect_template: None,
		note: None,
		favicon: None,
		thumbnail: None,
		favicon_mode: None,
		thumbnail_mode: None,
		starred: None,
		is_archived: None,
	}
}

/// The trash must never hold two bookmarks with the same URL. The
/// delete → re-add → delete cycle (each `remove` moves a fresh row to the
/// trash) would otherwise stack stale copies; each new removal purges the
/// older trashed one, leaving only the newest.
#[test]
fn trash_purges_older_trashed_copy_of_the_same_url() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let id1 = bm_db::insert(&conn, &plain_bookmark("https://youtube.com/@ch")).unwrap();
	assert!(bm_db::trash(&conn, id1).unwrap());

	// Re-adding the same URL succeeds — the trashed copy is outside the
	// partial unique index.
	let id2 = bm_db::insert(&conn, &plain_bookmark("https://youtube.com/@ch")).unwrap();
	assert!(bm_db::trash(&conn, id2).unwrap());

	let trashed = bm_db::list(
		&conn,
		&BookmarkFilter {
			trash: true,
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(trashed.len(), 1, "trash holds at most one copy per URL");
	assert_eq!(trashed[0].id, id2, "the newest trashed copy wins");
}

/// The single-id, id-list, and filter bulk trash paths all keep the trash
/// deduplicated (each funnels through `trash_with_dedup`).
#[test]
fn bulk_trash_paths_dedup_trash_too() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let trash_len = || {
		bm_db::list(
			&conn,
			&BookmarkFilter {
				trash: true,
				..Default::default()
			},
		)
		.unwrap()
		.len()
	};

	let a1 = bm_db::insert(&conn, &plain_bookmark("https://dup.example/a")).unwrap();
	bm_db::trash(&conn, a1).unwrap();
	let a2 = bm_db::insert(&conn, &plain_bookmark("https://dup.example/a")).unwrap();
	let res = bm_db::remove_ids(&conn, &[a2], false).unwrap();
	assert_eq!(res.removed, 1);
	assert_eq!(
		trash_len(),
		1,
		"remove_ids trashed a2 and purged the old copy"
	);

	bm_db::insert(&conn, &plain_bookmark("https://dup.example/a")).unwrap();
	let res = bm_db::remove_matching(
		&conn,
		&BookmarkFilter {
			..Default::default()
		},
		false,
	)
	.unwrap();
	assert_eq!(res.removed, 1, "only the live copy matches the filter");
	assert_eq!(
		trash_len(),
		1,
		"remove_matching trashed a3 and purged the old copy"
	);
}

/// Restoring a trashed bookmark whose URL is already owned by a live row is
/// a friendly error naming the owner — not a raw UNIQUE-constraint failure
/// (a re-added URL can coexist with its trashed predecessor, but the two
/// cannot both come back to life).
#[test]
fn restore_refuses_when_the_url_is_taken_by_a_live_bookmark() {
	silence_logs();
	let (_dir, path) = temp_db();
	let conn = open(&path).unwrap();

	let id1 = bm_db::insert(&conn, &plain_bookmark("https://youtube.com/@ch")).unwrap();
	bm_db::trash(&conn, id1).unwrap();
	let id2 = bm_db::insert(&conn, &plain_bookmark("https://youtube.com/@ch")).unwrap();

	let err = bm_db::restore(&conn, id1).unwrap_err();
	assert!(
		err.to_string()
			.contains(&format!("URL already exists as bookmark #{id2}")),
		"unexpected error: {err}"
	);

	// Restoring the live sibling (its own trashed re-add) still works.
	assert!(bm_db::trash(&conn, id2).unwrap());
	assert!(bm_db::restore(&conn, id2).unwrap());
	let active = bm_db::list(&conn, &BookmarkFilter::default()).unwrap();
	assert_eq!(active.len(), 1);
	assert_eq!(active[0].id, id2);
}

/// Backup, size stats, and the weekly maintenance pass all run on the pool
/// without taking the writer: `backup` uses its own read-only connection and
/// `stats`/`run_maintenance` a pooled reader, so an in-flight write can't be
/// blocked by them.
#[test]
fn backup_stats_and_maintenance_do_not_need_the_writer() {
	silence_logs();
	let (_dir, path) = temp_db();
	let db = Db::open(&path).unwrap();

	// A writer that is genuinely held (never released) must not stop the
	// snapshot, the gauges, or the health pass.
	let held = db.writer();
	bm_db::insert(&held, &plain_bookmark("https://example.com")).unwrap();

	let dest = _dir.path().join("snapshot.sqlite");
	db.backup(&dest).unwrap();
	let verify = open(&dest).unwrap();
	assert!(!table_names(&verify).is_empty());
	drop(verify);

	let stats = db.stats();
	assert!(stats.file_bytes > 0);
	assert!(stats.page_count > 0);

	db.run_maintenance().unwrap();
	drop(held);
}
