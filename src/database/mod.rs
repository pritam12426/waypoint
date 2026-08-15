/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! SQLite persistence layer.
//!
//! The HTTP layer uses the `Db` pool — one writer connection plus a
//! round-robin set of readers (WAL makes concurrent readers + a writer
//! safe). Connections are only ever touched inside
//! `tokio::task::spawn_blocking`; never share a raw `Connection` across
//! tasks directly — see the project notes in AGENTS.md.
//!
//! Schema policy: one idempotent init script
//! (`migrations/0001_init.up.sql`, run via `migrations::init`) applied on
//! every startup — no versioned migrations, no tracking table. `open()` is
//! the only entry point. Pool readers are opened *after* the writer has
//! finished initializing, so they never see a partial schema.
//!
//! # Layout
//!
//! * `bookmarks` — the core CRUD + filters + FTS search
//! * `tags` / `categories` — the two taxonomies and the bookmark↔tag links
//! * `visits` — visit tracking and the visit-derived stats
//! * `stats` — aggregate queries (overview, activity, hygiene, ...)
//! * `migrations` — the one-shot idempotent schema initializer
//!
//! Everything here returns `anyhow::Result`; no SQL is duplicated between
//! handlers because they all call these functions.

pub mod bookmarks;
pub mod categories;
pub mod migrations;
pub mod stats;
pub mod tags;
pub mod visits;

#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

/// How many read connections the HTTP pool holds. WAL allows many readers
/// alongside the single writer; this is the concurrency ceiling for
/// parallel page loads in the web UI (each reader round-robins through the
/// other handlers' queries).
pub const READ_POOL_SIZE: usize = 4;

/// A connection pool for the HTTP layer: one writer (every INSERT/UPDATE/
/// DELETE plus anything mixed read/write) and a round-robin set of readers
/// (list / count / search / stats).
///
/// WAL is what makes this safe: readers see a consistent snapshot while the
/// writer commits, so a page load never blocks a visit write. `open()`
/// still returns a single plain `Connection` for one-shot callers (imports,
/// schema init); the pool is the long-lived serving shape.
#[derive(Debug)]
pub struct Db {
	writer: Mutex<Connection>,
	readers: Vec<Mutex<Connection>>,
	next: AtomicUsize,
}

impl Db {
	/// Opens the database (the writer runs the schema init + seed) and spawns
	/// `READ_POOL_SIZE` reader connections against the same file. Readers
	/// open *after* the writer is fully migrated, so they never see a
	/// half-applied schema.
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		let path = path.as_ref();
		let writer = open(path)?;
		let readers = (0..READ_POOL_SIZE)
			.map(|_| open_reader(path))
			.collect::<Result<Vec<_>>>()?;
		crate::log_debug!(
			"opened connection pool (1 writer + {} readers) for {}",
			READ_POOL_SIZE,
			path.display()
		);
		Ok(Self {
			writer: Mutex::new(writer),
			readers: readers.into_iter().map(Mutex::new).collect(),
			next: AtomicUsize::new(0),
		})
	}

	/// An in-memory pool for unit tests. SQLite `file:` URIs need the
	/// `SQLITE_OPEN_URI` flag; `mode=memory&cache=shared` makes every
	/// connection in the pool share one in-memory database, so readers see
	/// what the writer committed — the same consistency shape the on-disk
	/// WAL pool provides for the tests that need it.
	#[cfg(test)]
	pub fn in_memory() -> Result<Self> {
		use rusqlite::OpenFlags;
		let uri = "file:waypoint-test?mode=memory&cache=shared";
		let flags = OpenFlags::default() | OpenFlags::SQLITE_OPEN_URI;
		let writer = open_with_flags(uri, flags)?;
		let readers = (0..READ_POOL_SIZE)
			.map(|_| open_reader_with_flags(uri))
			.collect::<Result<Vec<_>>>()?;
		Ok(Self {
			writer: Mutex::new(writer),
			readers: readers.into_iter().map(Mutex::new).collect(),
			next: AtomicUsize::new(0),
		})
	}

	/// The single writer connection. Every mutation and any mixed
	/// read-then-write handler (bulk delete, empty trash) goes here.
	pub fn writer(&self) -> MutexGuard<'_, Connection> {
		self.writer.lock().unwrap()
	}

	/// A read-only connection, round-robined across the pool so concurrent
	/// page loads spread across the readers instead of queueing on one.
	pub fn reader(&self) -> MutexGuard<'_, Connection> {
		let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.readers.len();
		self.readers[idx].lock().unwrap()
	}

	/// Merges the WAL into the main database file (`TRUNCATE` mode). Best
	/// effort: skips if the writer is momentarily held by an in-flight task.
	/// Called on graceful server shutdown so the `-wal`/`-shm` sidecars are
	/// empty (and then deleted by the last connection close) instead of
	/// being left with pages to replay.
	pub fn checkpoint(&self) {
		if let Ok(writer) = self.writer.try_lock() {
			let _ = writer.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
		}
	}

	/// Periodic WAL maintenance: a `PASSIVE` checkpoint that merges committed
	/// WAL frames into the main file without blocking readers or the writer
	/// (it returns immediately if any connection is mid-query). Unlike the
	/// shutdown `TRUNCATE` checkpoint this never blocks progress — it just
	/// keeps the WAL from growing without bound between restarts.
	pub fn wal_checkpoint_passive(&self) {
		if let Ok(writer) = self.writer.try_lock() {
			let _ = writer.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
		}
	}

	/// Whether the writer connection is currently held by an in-flight task.
	/// Feeds the `busy` gauge so an operator can see write saturation.
	pub fn writer_locked(&self) -> bool {
		self.writer.try_lock().is_err()
	}

	/// How many of the pool's readers are currently held. Feeds the `busy`
	/// gauge for read saturation.
	pub fn readers_in_use(&self) -> usize {
		self.readers
			.iter()
			.filter(|r| r.try_lock().is_err())
			.count()
	}

	/// Writes a consistent `VACUUM INTO` snapshot of the database to
	/// `dest` (an absolute path). Best effort like the checkpoints: skips if
	/// the writer is held. `VACUUM INTO` produces a fresh, fully-rewritten
	/// database file — no WAL sidecars, no uncheckpointed frames — so a
	/// backup can be copied/stored as a single self-contained file.
	pub fn backup(&self, dest: &Path) -> Result<()> {
		let writer = self
			.writer
			.lock()
			.map_err(|_| anyhow::anyhow!("writer poisoned"))?;
		writer
			.execute_batch(&format!(
				"VACUUM INTO '{}';",
				dest.display().to_string().replace('\'', "''")
			))
			.with_context(|| format!("VACUUM INTO backup to {} failed", dest.display()))?;
		Ok(())
	}
}

/// The prefix every automated backup file is named with (before the
/// local-time `YYYYMMDD-HHMMSS` stamp). Retention/pruning matches on it so
/// a user's unrelated files in the backup dir are left alone.
pub const BACKUP_PREFIX: &str = "waypointd-backup-";

/// Returns the file name for an automated backup at the given local time.
pub fn backup_filename(now: &chrono::DateTime<chrono::Local>) -> String {
	format!("{BACKUP_PREFIX}{}.sqlite", now.format("%Y%m%d-%H%M%S"))
}

/// Deletes the oldest automated backups in `dir` until at most `keep` remain.
/// Best effort: a file that cannot be removed is logged and skipped rather
/// than failing the backup cycle. Returns how many files were pruned.
pub fn prune_backups(dir: &Path, keep: usize) -> usize {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return 0;
	};
	let mut backups: Vec<std::path::PathBuf> = entries
		.filter_map(|e| e.ok())
		.map(|e| e.path())
		.filter(|p| {
			p.is_file()
				&& p.file_name()
					.map(|n| n.to_string_lossy().starts_with(BACKUP_PREFIX))
					.unwrap_or(false)
		})
		.collect();
	if backups.len() <= keep {
		return 0;
	}
	// Oldest first (file names sort lexicographically by the timestamp).
	backups.sort();
	let mut pruned = 0;
	for stale in backups.iter().take(backups.len().saturating_sub(keep)) {
		match std::fs::remove_file(stale) {
			Ok(()) => {
				crate::log_debug!("pruned old backup {}", stale.display());
				pruned += 1;
			}
			Err(err) => crate::log_warn!("could not prune backup {}: {err}", stale.display()),
		}
	}
	pruned
}

/// The FTS-syncing trigger names from the pre-archive era. Dropping them is
/// a required half of upgrading a legacy database: `CREATE TRIGGER IF NOT
/// EXISTS` would leave the old unguarded definitions in place, and
/// `ALTER TABLE ... RENAME COLUMN` refuses to rewrite a trigger whose body
/// writes into an FTS5 virtual table.
///
/// These are the *old* trigger names a pre-versioned build could have
/// installed; the schema init script then re-creates its own guarded set
/// with the same names via `CREATE TRIGGER IF NOT EXISTS`.
const LEGACY_FTS_TRIGGERS: &[&str] = &[
	"bookmarks_fts_insert",
	"bookmarks_fts_delete",
	"bookmarks_fts_update",
	"bookmarks_fts_trash",
	"bookmarks_fts_restore",
	"bookmarks_fts_soft_delete",
	"bookmarks_fts_archived_insert",
	"bookmarks_fts_archived_delete",
	"bookmarks_fts_archived_update",
	"bookmarks_fts_archived_trash",
	"bookmarks_fts_archived_restore",
	"bookmarks_fts_archive",
	"bookmarks_fts_unarchive",
];

/// Opens (creating if needed) the waypoint database at `path`, applies the
/// idempotent schema init script, and seeds the default category. Returns a
/// ready-to-use connection.
///
/// The repair pipeline for a database that predates versioning is:
/// `legacy_preclean` (make the old schema match what the init script
/// expects) → `migrations::init` (the same idempotent batch, safe on both
/// fresh and legacy DBs) → `legacy_postclean` (repair search-index state
/// the old triggers let rot).
pub fn open<P: AsRef<Path>>(path: P) -> Result<Connection> {
	open_with_flags(path.as_ref(), rusqlite::OpenFlags::default())
}

/// [`open`] with explicit open flags — the one test-only caller (the
/// in-memory pool) needs `SQLITE_OPEN_URI` for shared-cache `file:` URIs.
fn open_with_flags<P: AsRef<Path>>(path: P, flags: rusqlite::OpenFlags) -> Result<Connection> {
	let path = path.as_ref();
	crate::log_debug!("opening database at {}", path.display());
	let mut conn = Connection::open_with_flags(path, flags)
		.with_context(|| format!("failed to open database at {}", path.display()))?;
	apply_pragmas(&mut conn)?;

	// Detect pre-versioned databases so they get the pre/post clean passes
	// around the schema init batch.
	let legacy = is_legacy_schema(&conn)?;
	if legacy {
		crate::log_info!(
			"legacy database detected at {} — running pre/post schema repair",
			path.display()
		);
		legacy_preclean(&conn).context("failed to repair legacy schema (pre-init)")?;
	}

	migrations::init(&mut conn).context("failed to initialize schema")?;

	if legacy {
		legacy_postclean(&conn).context("failed to repair legacy schema (post-init)")?;
	}

	categories::ensure_default(&conn).context("failed to seed default category")?;

	crate::log_debug!("database ready (schema applied, default category seeded)");
	Ok(conn)
}

/// Opens a pooled reader connection: same pragmas as the writer, but no
/// schema init or seeding — the writer is guaranteed to have run them first
/// (`Db::open` opens readers only after the writer is ready).
fn open_reader(path: &Path) -> Result<Connection> {
	crate::log_trace!("opening pooled reader connection for {}", path.display());
	let mut conn = Connection::open(path)?;
	apply_pragmas(&mut conn)?;
	Ok(conn)
}

/// Test-only twin of [`open_reader`] that enables `file:` URI support (used
/// by the shared-cache in-memory pool).
#[cfg(test)]
fn open_reader_with_flags(uri: &str) -> Result<Connection> {
	use rusqlite::OpenFlags;
	let mut conn =
		Connection::open_with_flags(uri, OpenFlags::default().union(OpenFlags::SQLITE_OPEN_URI))?;
	apply_pragmas(&mut conn)?;
	Ok(conn)
}

/// The shared connection setup for every connection in the process. Must be
/// set outside any transaction.
fn apply_pragmas(conn: &mut Connection) -> Result<()> {
	// Must be set outside any transaction — it is a no-op inside one — and
	// before the schema init batch touches data. FK enforcement is what makes
	// `ON DELETE CASCADE` (tags, bookmark_tags) and the category reassignment
	// in `categories::delete` behave correctly.
	conn.pragma_update(None, "foreign_keys", true)
		.context("failed to enable foreign_keys pragma")?;
	// A few seconds of grace lets a second process (e.g. two `serve`
	// instances) wait out a brief lock instead of erroring instantly.
	conn.busy_timeout(std::time::Duration::from_secs(5))
		.context("failed to set busy timeout")?;

	// WAL: readers proceed while the writer commits — the one change that
	// makes a read-heavy web UI coexist with background writes. It persists
	// in the database file (later opens keep it) but we set it here so a
	// legacy database flips over the moment it is opened. Must be set
	// outside any transaction.
	conn.pragma_update(None, "journal_mode", "WAL")
		.context("failed to enable WAL journal mode")?;
	// synchronous=NORMAL is the durability/speed trade-off SQLite recommends
	// for WAL: a power loss may lose the last transactions but never corrupts
	// the database (the WAL is rebuilt from the last checkpoint).
	conn.pragma_update(None, "synchronous", "NORMAL")
		.context("failed to set synchronous pragma")?;
	// ~32 MiB page cache shared across connections in-process.
	conn.pragma_update(None, "cache_size", -32768_i64)
		.context("failed to set cache_size pragma")?;
	// temp_store=MEMORY keeps the ORDER BY / GROUP BY temp b-trees (the
	// pre-index list sorts and the stats aggregates) in RAM instead of
	// spilling to disk.
	conn.pragma_update(None, "temp_store", "MEMORY")
		.context("failed to set temp_store pragma")?;
	// Up to 256 MiB of the database is mapped read-only, so reads avoid
	// page-cache syscalls entirely.
	conn.pragma_update(None, "mmap_size", 268_435_456_i64)
		.context("failed to set mmap_size pragma")?;
	Ok(())
}

/// A "legacy" database is one written by the pre-versioned waypoint builds:
/// it already has the `bookmarks` table carrying the old `deleted_at`
/// recycle-bin column (every schema since then uses `trashed_at`).
///
/// Checking `sqlite_master` and the column list (not data) means an
/// empty-but-existing schema is classified correctly, and a truly empty file
/// (no tables at all) is fresh — the init script builds everything, no
/// legacy cleanup needed.
fn is_legacy_schema(conn: &Connection) -> Result<bool> {
	if !has_table(conn, "bookmarks")? {
		return Ok(false);
	}
	has_column(conn, "bookmarks", "deleted_at")
}

/// Whether a table with `name` exists in the schema.
pub(crate) fn has_table(conn: &Connection, name: &str) -> Result<bool> {
	let exists: bool = conn
		.query_row(
			"SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
			params![name],
			|_| Ok(true),
		)
		.optional()?
		.unwrap_or(false);
	Ok(exists)
}

/// Whether `table` currently has `column` — used by the schema init to
/// conditionally add the redirect-template column and by the legacy upgrade
/// to conditionally rename/drop columns that only some old builds created.
pub(crate) fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
	let found: bool = conn
		.query_row(
			"SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
			params![table, column],
			|_| Ok(true),
		)
		.optional()?
		.unwrap_or(false);
	Ok(found)
}

/// Pre-init half of the legacy upgrade: make the old schema look like the
/// state the init script expects to build on. Runs only for pre-versioned
/// databases (marked by the `deleted_at` column).
fn legacy_preclean(conn: &Connection) -> Result<()> {
	// Drop the old FTS trigger set so the init script's guarded definitions
	// install cleanly (they would otherwise be shadowed by IF NOT EXISTS).
	for trigger in LEGACY_FTS_TRIGGERS {
		conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger};"))?;
	}

	// Recycle-bin column was `deleted_at` before the trash vocabulary
	// settled on `trashed_at`. Renaming is blocked while a trigger writes
	// to FTS, hence the drops above.
	if has_column(conn, "bookmarks", "deleted_at")? {
		conn.execute_batch("ALTER TABLE bookmarks RENAME COLUMN deleted_at TO trashed_at;")
			.context("failed to rename deleted_at column to trashed_at")?;
		crate::log_info!("legacy upgrade: renamed bookmarks.deleted_at -> trashed_at");
	}

	// Dead schema baggage from an early build — never auto-populated, no
	// filtering logic ever used it.
	if has_column(conn, "bookmarks", "mime_type")? {
		conn.execute_batch("ALTER TABLE bookmarks DROP COLUMN mime_type;")
			.context("failed to drop mime_type column")?;
		crate::log_info!("legacy upgrade: dropped bookmarks.mime_type");
	}

	Ok(())
}

/// Post-init half of the legacy upgrade: repair search-index state that
/// the old triggers let rot.
///
/// Trash cleanup: databases predating the `bookmarks_fts_trash` trigger may
/// still have trashed rows in the main index. Each stale row is removed
/// with the FTS5 special `delete` command (a plain DELETE would corrupt an
/// external-content index). This is safe only in the one-shot legacy path —
/// on a trigger-consistent database `delete` on an absent row errors.
///
/// Archive rebuild: the pre-archive triggers routed every row (archived and
/// trashed alike) into `bookmarks_fts`. If any are still sitting there, both
/// indexes are wiped and re-derived from `bookmarks`.
fn legacy_postclean(conn: &Connection) -> Result<()> {
	// 1. Scrub stale trashed rows from the main index. The FTS5 `delete`
	// command needs the *exact* indexed values, so we read the four indexed
	// columns for every trashed row and feed them back.
	let mut stmt = conn.prepare(
		"SELECT id, title, description, note, url
		 FROM bookmarks WHERE trashed_at IS NOT NULL",
	)?;
	let stale: Vec<StaleFtsRow> = {
		let rows = stmt.query_map([], |row| {
			Ok(StaleFtsRow {
				id: row.get(0)?,
				title: row.get(1)?,
				description: row.get(2)?,
				note: row.get(3)?,
				url: row.get(4)?,
			})
		})?;
		rows.collect::<rusqlite::Result<_>>()?
	};
	let cleaned = stale.len();
	for row in stale {
		conn.execute(
			"INSERT INTO bookmarks_fts(bookmarks_fts, rowid, title, description, note, url)
			 VALUES ('delete', ?1, ?2, ?3, ?4, ?5)",
			rusqlite::params![row.id, row.title, row.description, row.note, row.url],
		)?;
	}
	if cleaned > 0 {
		crate::log_info!(
			"legacy upgrade: removed {cleaned} stale trashed entries from the search index"
		);
	}

	// 2. If archived/trashed rows leaked into the main index, a full rebuild
	// of both indexes is the only clean fix (a targeted delete is impossible
	// for rows whose exact old content we can't reconstruct). `delete-all`
	// wipes, then both indexes are re-derived from the current bookmarks.
	let stale_archived: i64 = conn.query_row(
		"SELECT COUNT(*) FROM bookmarks_fts f
		 JOIN bookmarks b ON b.id = f.rowid
		 WHERE b.trashed_at IS NOT NULL OR b.is_archived = 1",
		[],
		|row| row.get(0),
	)?;
	if stale_archived > 0 {
		conn.execute_batch(
			"INSERT INTO bookmarks_fts(bookmarks_fts) VALUES ('delete-all');
			 INSERT INTO bookmarks_fts_archived(bookmarks_fts_archived) VALUES ('delete-all');
			 INSERT INTO bookmarks_fts(rowid, title, description, note, url)
				 SELECT id, title, description, note, url FROM bookmarks
				 WHERE trashed_at IS NULL AND is_archived = 0;
			 INSERT INTO bookmarks_fts_archived(rowid, title, description, note, url)
				 SELECT id, title, description, note, url FROM bookmarks
				 WHERE trashed_at IS NULL AND is_archived = 1;",
		)?;
		crate::log_info!(
			"legacy upgrade: rebuilt both search indexes ({stale_archived} stale entries)"
		);
	}

	Ok(())
}

/// A bookmark row as read by the legacy search-index cleanup. The FTS5
/// `delete` command needs the exact indexed content to remove a row, so all
/// four indexed columns are carried alongside the id.
struct StaleFtsRow {
	id: i64,
	title: String,
	description: Option<String>,
	note: Option<String>,
	url: String,
}
