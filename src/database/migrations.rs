/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! The versioned, forward-only migration runner.
//!
//! # Why versioned migrations?
//!
//! The original design re-ran one big `001_initial.sql` on every startup —
//! idempotent, but it made *evolution* impossible: a changed table could
//! only be "fixed" by re-running CREATE statements that silently no-op on
//! an existing database. The current design records what has run in
//! `schema_migrations`, applies each pending migration exactly once (each
//! in its own transaction), and never looks back.
//!
//! # Adding a migration
//!
//! 1. Write `migrations/NNNN_name.up.sql` (idempotent — legacy databases
//!    run through the same batch).
//! 2. Add one `Migration` entry to `MIGRATIONS`.
//!
//! That's the whole integration; nothing else in the codebase changes.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

/// A single forward-only migration. `up` runs inside a transaction and must
/// leave the database at exactly one version step higher.
pub struct Migration {
	pub version: i64,
	pub name: &'static str,
	pub up: &'static str,
}

/// All migrations, ordered by version. A new migration is one new entry
/// here plus one new `migrations/NNNN_name.up.sql` file — nothing else has
/// to change anywhere in the codebase.
///
/// The SQL is `include_str!`'d at compile time, so a migration can never
/// go missing from a deployed binary — the bytes ship inside it.
pub const MIGRATIONS: &[Migration] = &[
	Migration {
		version: 1,
		name: "init",
		up: include_str!("migrations/0001_init.up.sql"),
	},
	Migration {
		version: 2,
		name: "scale_indexes",
		up: include_str!("migrations/0002_scale_indexes.up.sql"),
	},
	Migration {
		version: 3,
		name: "keyword_collation",
		up: include_str!("migrations/0003_keyword_collation.up.sql"),
	},
];

/// Latest schema version this build knows how to reach.
pub fn current_version() -> i64 {
	MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0)
}

/// Creates the tracking table if needed. It's deliberately NOT a migration
/// itself: `apply` needs somewhere to record version 1 *before* it can run
/// migration 1, so the table is bootstrapped here outside the migration
/// list.
fn ensure_tracking_table(conn: &Connection) -> Result<()> {
	conn.execute_batch(
		"CREATE TABLE IF NOT EXISTS schema_migrations (
			version    INTEGER PRIMARY KEY,
			name       TEXT NOT NULL,
			applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
		);",
	)
	.context("failed to create schema_migrations table")?;
	Ok(())
}

/// Applies every pending migration. Each migration runs in its own
/// transaction: a failure rolls back that migration's batch entirely, but
/// leaves earlier, already-applied versions alone.
///
/// Reads the highest applied version from `schema_migrations`, then runs
/// every `MIGRATIONS` entry above it in version order, recording each one
/// in the same transaction that applied it.
pub fn apply(conn: &mut Connection) -> Result<()> {
	ensure_tracking_table(conn)?;

	let applied: i64 = conn.query_row(
		"SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
		[],
		|row| row.get(0),
	)?;

	let pending: Vec<&Migration> = MIGRATIONS.iter().filter(|m| m.version > applied).collect();

	if pending.is_empty() {
		crate::log_trace!("migrations: database is current (version {applied})");
		return Ok(());
	}

	for migration in pending {
		crate::log_info!(
			"applying migration {} ({})",
			migration.version,
			migration.name
		);
		let tx = conn.transaction().with_context(|| {
			format!(
				"failed to begin transaction for migration {}",
				migration.version
			)
		})?;
		tx.execute_batch(migration.up).with_context(|| {
			format!(
				"failed to apply migration {} ({})",
				migration.version, migration.name
			)
		})?;
		// Record the version *inside* the same transaction: if the migration
		// SQL succeeds but this insert fails, the rollback undoes both.
		tx.execute(
			"INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
			params![migration.version, migration.name],
		)
		.with_context(|| {
			format!(
				"failed to record migration {} ({})",
				migration.version, migration.name
			)
		})?;
		tx.commit()?;
	}

	crate::log_info!("migrations: database at version {}", current_version());
	Ok(())
}

/// Highest version recorded in `schema_migrations`, for diagnostics and
/// tests.
pub fn applied_version(conn: &Connection) -> Result<i64> {
	conn.query_row(
		"SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
		[],
		|row| row.get(0),
	)
	.context("failed to read applied migration version")
}
