/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! The schema initializer.
//!
//! There is exactly one schema file (`migrations/0001_init.up.sql`), embedded
//! via `include_str!` and re-run on every startup. Every statement in it is
//! idempotent (`CREATE ... IF NOT EXISTS` / `DROP ... IF EXISTS`), so
//! re-running it on a database that already has the schema is a no-op — and
//! the legacy-upgrade path routes pre-versioned databases through the same
//! batch. Evolving the schema means editing that one file, nothing more.
//!
//! The one thing SQLite can't express idempotently is `ALTER TABLE ... ADD
//! COLUMN`. The `redirect_template` column is folded into
//! the `CREATE TABLE` for fresh databases, so pre-existing `bookmarks` tables
//! (which lack it) get it added here, guarded by a column check, before the
//! batch runs.

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::{has_column, has_table};

/// Applies the schema to `conn`. Safe to call on every startup.
pub fn init(conn: &mut Connection) -> Result<()> {
	if has_table(conn, "bookmarks")? && !has_column(conn, "bookmarks", "redirect_template")? {
		conn.execute_batch("ALTER TABLE bookmarks ADD COLUMN redirect_template TEXT DEFAULT NULL;")
			.context("failed to add bookmarks.redirect_template column")?;
	}

	conn.execute_batch(include_str!("migrations/0001_init.up.sql"))
		.context("failed to apply schema init script")?;
	crate::log_debug!("schema initialized");
	Ok(())
}
