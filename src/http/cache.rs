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
//! categories are written, so the HTTP layer caches it briefly. The stats
//! endpoints cache their pre-serialized JSON bodies the same way.
//!
//! The cache lives in `AppState`, not the database layer: the server is
//! long-lived so caching pays off, and the HTTP integration tests build a
//! fresh `AppState` per test, so cached values can never leak from one
//! test's database into another's.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Entry<T> {
	at: Instant,
	value: T,
}

/// A TTL'd `String` → `T` cache. Writes invalidate wholesale: a bookmark,
/// tag, or category mutation can change any filter's count and any
/// aggregate, so the safe move is to drop everything and let the next read
/// recompute the entries it actually needs.
// ponytail: eager per-entry refresh was removed (it recomputed every warm
// entry on every write); invalidate-on-write does the same total work,
// lazily, with ~90 lines less machinery.
pub struct Cache<T> {
	inner: Mutex<HashMap<String, Entry<T>>>,
	ttl: Duration,
	/// When set, `put` drops the oldest entry once the map exceeds this.
	/// Client-chosen keys (filter strings, limits, offsets) can otherwise
	/// grow the map without bound over a TTL window.
	max_entries: Option<usize>,
}

impl<T> Cache<T> {
	pub fn new(ttl: Duration) -> Self {
		Self {
			inner: Mutex::new(HashMap::new()),
			ttl,
			max_entries: None,
		}
	}

	/// A cache that drops its oldest entry once `max_entries` is exceeded.
	pub fn bounded(ttl: Duration, max_entries: usize) -> Self {
		Self {
			inner: Mutex::new(HashMap::new()),
			ttl,
			max_entries: Some(max_entries),
		}
	}

	/// Returns the cached value if present and younger than the TTL.
	pub fn get(&self, key: &str) -> Option<T>
	where
		T: Clone,
	{
		let hit = {
			let inner = self.inner.lock().unwrap();
			match inner.get(key) {
				Some(entry) if entry.at.elapsed() < self.ttl => Some(entry.value.clone()),
				_ => None,
			}
		};
		crate::log_trace!(
			"cache {} for {key:?}",
			if hit.is_some() { "hit" } else { "miss" }
		);
		hit
	}

	pub fn put(&self, key: &str, value: T) {
		let mut inner = self.inner.lock().unwrap();
		inner.insert(
			key.to_owned(),
			Entry {
				at: Instant::now(),
				value,
			},
		);
		if let Some(max) = self.max_entries
			&& inner.len() > max
			&& let Some(oldest) = inner
				.iter()
				.min_by_key(|(_, e)| e.at)
				.map(|(k, _)| k.clone())
		{
			inner.remove(&oldest);
		}
		crate::log_trace!("cache stored for {key:?}");
	}

	/// Drop every entry.
	pub fn invalidate(&self) {
		let dropped = self.inner.lock().unwrap().len();
		self.inner.lock().unwrap().clear();
		crate::log_trace!("cache invalidated ({dropped} entries dropped)");
	}
}

impl<T> Default for Cache<T> {
	fn default() -> Self {
		Self::new(Duration::from_secs(5))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn get_obeys_ttl() {
		let cache = Cache::new(Duration::from_secs(5));
		assert_eq!(cache.get("k"), None);
		cache.put("k", 7);
		assert_eq!(cache.get("k"), Some(7));
		cache.invalidate();
		assert_eq!(cache.get("k"), None);
	}

	#[test]
	fn bounded_evicts_oldest() {
		let cache = Cache::bounded(Duration::from_secs(5), 2);
		cache.put("a", 1);
		cache.put("b", 2);
		cache.put("c", 3);
		assert_eq!(cache.get("a"), None);
		assert_eq!(cache.get("b"), Some(2));
		assert_eq!(cache.get("c"), Some(3));
	}
}
