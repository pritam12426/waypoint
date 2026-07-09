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
