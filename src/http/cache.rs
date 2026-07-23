/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Small time-based caches for the HTTP layer.
//!
//! Every bookmarks page load runs two queries: `list` (bounded by LIMIT)
//! and `count` (a full `COUNT(*)`, ~250ms at 1M rows — see the bench harness
//! in `examples/bench.rs`). The count only changes when bookmarks, tags, or
//! categories are written, so the HTTP layer caches it briefly. Each entry
//! carries the closure that recomputes it, so a successful write can
//! *refresh* the warm entries in place instead of just dropping them: the
//! cache in RAM reflects the new data immediately and the next read is still
//! a hit.
//!
//! The cache lives in `AppState`, not the database layer: the server is
//! long-lived so caching pays off, and the HTTP integration tests build a
//! fresh `AppState` per test, so cached values can never leak from one
//! test's database into another's.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;

/// Recomputes a cached count against a database connection. Stored with the
/// entry at put time — the cache itself doesn't know how to query, the
/// caller does.
type CountRefresher = Arc<dyn Fn(&Connection) -> anyhow::Result<i64> + Send + Sync + 'static>;

struct CountEntry {
	at: Instant,
	value: i64,
	refresh: CountRefresher,
}

/// `bookmarks::count` results keyed by the filter's canonical string form
/// (pagination fields stripped — `count` ignores them), with a short TTL.
pub struct CountCache {
	inner: Mutex<HashMap<String, CountEntry>>,
	ttl: Duration,
}
impl CountCache {
	pub fn new() -> Self {
		Self {
			inner: Mutex::new(HashMap::new()),
			ttl: Duration::from_secs(5),
		}
	}

	/// Returns the cached count if present and younger than the TTL.
	pub fn get(&self, key: &str) -> Option<i64> {
		let hit = {
			let inner = self.inner.lock().unwrap();
			match inner.get(key) {
				Some(entry) if entry.at.elapsed() < self.ttl => Some(entry.value),
				_ => None,
			}
		};
		crate::log_trace!(
			"count cache {} for {key:?}",
			if hit.is_some() { "hit" } else { "miss" }
		);
		hit
	}

	/// Stores a count together with the closure that recomputes it. The
	/// closure is what lets `refresh` update the entry after a write.
	pub fn put(&self, key: &str, value: i64, refresh: CountRefresher) {
		self.inner.lock().unwrap().insert(
			key.to_owned(),
			CountEntry {
				at: Instant::now(),
				value,
				refresh,
			},
		);
		crate::log_trace!("count cache stored {value} for {key:?}");
	}

	/// Recompute every cached entry in place against `conn` and reset its
	/// TTL, so a successful write leaves the cache reflecting the new data
	/// instead of going cold. Entries whose recompute fails are dropped —
	/// the next read rebuilds them from scratch. Queries run outside the
	/// cache lock (a snapshot of the refreshers is taken first) so a slow
	/// recompute never blocks a concurrent `get`/`put`.
	pub fn refresh(&self, conn: &Connection) {
		let refreshers: Vec<(String, CountRefresher)> = {
			let inner = self.inner.lock().unwrap();
			inner
				.iter()
				.map(|(k, e)| (k.clone(), e.refresh.clone()))
				.collect()
		};
		let n = refreshers.len();
		for (key, refresh) in refreshers {
			match refresh(conn) {
				Ok(value) => {
					let mut inner = self.inner.lock().unwrap();
					// Only touch an entry that still exists — a concurrent
					// `get`/`put` may have replaced or dropped it while the
					// recompute ran, and a stale value must not resurrect it.
					if let Some(entry) = inner.get_mut(&key) {
						entry.at = Instant::now();
						entry.value = value;
					}
				}
				Err(err) => {
					crate::log_debug!(
						"count cache refresh failed for {key:?}: {err:#} — dropping entry"
					);
					self.inner.lock().unwrap().remove(&key);
				}
			}
		}
		crate::log_trace!("count cache refreshed ({n} entries)");
	}

	/// Drop every entry. Used when a refresh can't run and by the visit
	/// tracking path, where the cache must not stall a redirect.
	pub fn invalidate(&self) {
		let dropped = self.inner.lock().unwrap().len();
		self.inner.lock().unwrap().clear();
		crate::log_trace!("count cache invalidated ({dropped} entries dropped)");
	}
}

impl Default for CountCache {
	fn default() -> Self {
		Self::new()
	}
}

/// Recomputes a cached stats body against a database connection. Mirrors
/// `CountRefresher` for the pre-serialized JSON bodies below.
type StatsRefresher = Arc<dyn Fn(&Connection) -> anyhow::Result<Vec<u8>> + Send + Sync + 'static>;

struct StatsEntry {
	at: Instant,
	body: Vec<u8>,
	refresh: StatsRefresher,
}

/// Pre-serialized JSON responses for the aggregate stats endpoints, keyed by
/// endpoint + pagination, with a longer TTL than the count cache. The stats
/// queries are full-corpus GROUP BY / ORDER BY passes (~1s at 1M rows — see
/// `examples/bench.rs`), so the dashboard rides this cache while a stale
/// count would be far more visible. Same test-isolation story as
/// `CountCache`: it lives in `AppState` and is refreshed alongside it.
pub struct StatsCache {
	inner: Mutex<HashMap<String, StatsEntry>>,
	ttl: Duration,
}

