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

impl StatsCache {
	pub fn new() -> Self {
		Self {
			inner: Mutex::new(HashMap::new()),
			ttl: Duration::from_secs(30),
		}
	}

	/// Returns a clone of the cached body if present and younger than the
	/// TTL (cloned so the lock drops before the body is served).
	pub fn get(&self, key: &str) -> Option<Vec<u8>> {
		let hit = {
			let inner = self.inner.lock().unwrap();
			match inner.get(key) {
				Some(entry) if entry.at.elapsed() < self.ttl => Some(entry.body.clone()),
				_ => None,
			}
		};
		crate::log_trace!(
			"stats cache {} for {key:?}{}",
			if hit.is_some() { "hit" } else { "miss" },
			match &hit {
				Some(body) => format!(" ({} bytes)", body.len()),
				None => String::new(),
			}
		);
		hit
	}

	/// Stores a pre-serialized body together with the closure that recomputes
	/// it, so `refresh` can update the entry in place after a write.
	pub fn put(&self, key: &str, body: Vec<u8>, refresh: StatsRefresher) {
		crate::log_trace!("stats cache stored ({} bytes) for {key:?}", body.len());
		self.inner.lock().unwrap().insert(
			key.to_owned(),
			StatsEntry {
				at: Instant::now(),
				body,
				refresh,
			},
		);
	}

	/// Recompute every cached body in place against `conn` and reset its TTL
	/// (see `CountCache::refresh` for the rationale and locking behavior).
	pub fn refresh(&self, conn: &Connection) {
		let refreshers: Vec<(String, StatsRefresher)> = {
			let inner = self.inner.lock().unwrap();
			inner
				.iter()
				.map(|(k, e)| (k.clone(), e.refresh.clone()))
				.collect()
		};
		let n = refreshers.len();
		for (key, refresh) in refreshers {
			match refresh(conn) {
				Ok(body) => {
					let mut inner = self.inner.lock().unwrap();
					if let Some(entry) = inner.get_mut(&key) {
						entry.at = Instant::now();
						entry.body = body;
					}
				}
				Err(err) => {
					crate::log_debug!(
						"stats cache refresh failed for {key:?}: {err:#} — dropping entry"
					);
					self.inner.lock().unwrap().remove(&key);
				}
			}
		}
		crate::log_trace!("stats cache refreshed ({n} entries)");
	}

	pub fn invalidate(&self) {
		let dropped = self.inner.lock().unwrap().len();
		self.inner.lock().unwrap().clear();
		crate::log_trace!("stats cache invalidated ({dropped} entries dropped)");
	}
}

impl Default for StatsCache {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A helper refresh that always answers `value`.
	fn fixed(value: i64) -> CountRefresher {
		Arc::new(move |_: &Connection| Ok(value))
	}

	#[test]
	fn count_cache_get_obeys_ttl() {
		let cache = CountCache::new();
		assert_eq!(cache.get("k"), None);
		cache.put("k", 7, fixed(7));
		assert_eq!(cache.get("k"), Some(7));
		cache.invalidate();
		assert_eq!(cache.get("k"), None);
	}

	#[test]
	fn count_cache_refresh_updates_value_and_stays_warm() {
		let cache = CountCache::new();
		cache.put("k", 1, Arc::new(|_: &Connection| Ok(2)));
		// `refresh` needs a live handle even when the refresher ignores it.
		let conn = Connection::open_in_memory().unwrap();
		cache.refresh(&conn);
		assert_eq!(cache.get("k"), Some(2));
	}

	#[test]
	fn count_cache_refresh_drops_failing_entries() {
		let cache = CountCache::new();
		cache.put("good", 1, fixed(1));
		cache.put(
			"bad",
			2,
			Arc::new(|_: &Connection| anyhow::bail!("query failed")),
		);
		let conn = Connection::open_in_memory().unwrap();
		cache.refresh(&conn);
		assert_eq!(cache.get("good"), Some(1));
		assert_eq!(cache.get("bad"), None);
	}

	#[test]
	fn stats_cache_refresh_updates_body() {
		let cache = StatsCache::new();
		cache.put(
			"hygiene",
			b"old".to_vec(),
			Arc::new(|_: &Connection| Ok(b"new".to_vec())),
		);
		let conn = Connection::open_in_memory().unwrap();
		cache.refresh(&conn);
		assert_eq!(cache.get("hygiene"), Some(b"new".to_vec()));
	}
}
