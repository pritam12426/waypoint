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
