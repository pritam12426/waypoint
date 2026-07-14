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
//! Migration policy: forward-only, versioned, tracked in
//! `schema_migrations`. `open()` is the only entry point and always leaves
//! the database at `migrations::current_version()`. Pool readers are opened
//! *after* the writer has finished migrating, so they never see a partial
//! schema.
//!
//! # Layout
//!
//! * `bookmarks` — the core CRUD + filters + FTS search
//! * `tags` / `categories` — the two taxonomies and the bookmark↔tag links
//! * `visits` — visit tracking and the visit-derived stats
//! * `stats` — aggregate queries (overview, activity, hygiene, ...)
//! * `migrations` — the versioned, forward-only migration runner
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
/// migrations); the pool is the long-lived serving shape.
#[derive(Debug)]
pub struct Db {
	writer: Mutex<Connection>,
	readers: Vec<Mutex<Connection>>,
	next: AtomicUsize,
}

impl Db {
	/// Opens the database (the writer runs migrations + seed) and spawns
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
/// installed; migration 0001 then re-creates its own guarded set with the
/// same names via `CREATE TRIGGER IF NOT EXISTS`.
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

/// Opens (creating if needed) the waypoint database at `path`, applies any
/// pending versioned migrations (upgrading a pre-versioned database along
/// the way), and seeds the default category. Returns a ready-to-use
/// connection.
///
/// The upgrade pipeline for a database that predates versioning is:
/// `legacy_preclean` (make the old schema match what 0001 expects) →
/// `migrations::apply` (the normal batch, which is idempotent and safe on
/// both fresh and legacy DBs) → `legacy_postclean` (repair search-index
/// state the old triggers let rot).
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
	// around the migration batch.
	let legacy = is_legacy_schema(&conn)?;
	if legacy {
		crate::log_info!(
			"legacy database detected at {} — running pre/post migration upgrade",
			path.display()
		);
		legacy_preclean(&conn).context("failed to upgrade legacy schema (pre-migration)")?;
	}

	migrations::apply(&mut conn).context("failed to apply migrations")?;

	if legacy {
		legacy_postclean(&conn).context("failed to upgrade legacy schema (post-migration)")?;
	}

	categories::ensure_default(&conn).context("failed to seed default category")?;

	crate::log_debug!("database ready (schema applied, default category seeded)");
	Ok(conn)
}

/// Opens a pooled reader connection: same pragmas as the writer, but no
/// migrations or seeding — the writer is guaranteed to have run them first
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
