/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! In-crate tests for the persistence layer: migrations, idempotent reopen,
//! and the legacy-database upgrade path.
//!
//! The legacy test is the important one — it rebuilds a *pre-versioned*
//! database from the current schema (old column name, no
//! `schema_migrations`, no FTS triggers, a trashed row leaking into the
//! main index) and proves `database::open` repairs all of it. That's the
//! safety net for real user data carried over from the old builds.

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

/// A brand-new database comes out at the latest schema version, with all
/// expected tables and the default category seeded.
#[test]
fn fresh_database_reaches_current_version() {
	silence_logs();
	let (_dir, path) = temp_db();
	{
		let conn = open(&path).expect("open fresh database");
		assert_eq!(
			migrations::applied_version(&conn).unwrap(),
			migrations::current_version()
		);
		let tables = table_names(&conn);
		for expected in [
			"bookmarks",
			"bookmarks_fts",
			"bookmarks_fts_archived",
			"bookmark_tags",
			"categories",
			"schema_migrations",
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

/// Reopening the same database is a no-op: version stays put and the
/// default-category seeding doesn't duplicate rows.
#[test]
fn reopen_is_idempotent() {
	silence_logs();
	let (_dir, path) = temp_db();
	{
		let conn = open(&path).unwrap();
		assert_eq!(
			migrations::applied_version(&conn).unwrap(),
			migrations::current_version()
		);
	}
	{
		let conn = open(&path).unwrap();
		assert_eq!(
			migrations::applied_version(&conn).unwrap(),
			migrations::current_version()
		);
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
	// no FTS triggers, no schema_migrations).
	{
		let mut conn = Connection::open(&path).unwrap();
		migrations::apply(&mut conn).unwrap();
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
		// Drop the schema_migrations tracking to simulate pre-versioned era.
		conn.execute_batch("DROP TABLE schema_migrations;").unwrap();
		// Drop FTS triggers (required before the column rename, same reason
		// as in legacy_preclean).
		for trigger in super::LEGACY_FTS_TRIGGERS {
			conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger};"))
				.unwrap();
		}
		conn.execute_batch("ALTER TABLE bookmarks RENAME COLUMN trashed_at TO deleted_at;")
			.unwrap();
	}

	// Reopening through the public entry point repairs everything.
	let conn = open(&path).expect("open legacy database");
	assert_eq!(
		migrations::applied_version(&conn).unwrap(),
		migrations::current_version()
	);

	// Column renamed back.
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
/// derives it from the URL (domain `/favicon.ico` fallback), and sites with
/// a thumbnail rule (YouTube) get a thumbnail too.
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
		Some("https://example.com/favicon.ico")
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
