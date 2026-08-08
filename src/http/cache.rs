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
//! categories are written, so the HTTP layer caches it briefly and each
//! write handler invalidates the cache explicitly.
//!
//! The cache lives in `AppState`, not the database layer: the CLI is
//! one-shot per process (caching buys nothing) and the HTTP integration
//! tests build a fresh `AppState` per test, so cached values can never leak
//! from one test's database into another's.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// `bookmarks::count` results keyed by the filter's canonical string form
/// (pagination fields stripped — `count` ignores them), with a short TTL.
#[derive(Debug)]
pub struct CountCache {
	inner: Mutex<HashMap<String, (Instant, i64)>>,
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
				Some((at, value)) if at.elapsed() < self.ttl => Some(*value),
				_ => None,
			}
		};
		crate::log_trace!(
			"count cache {} for {key:?}",
			if hit.is_some() { "hit" } else { "miss" }
		);
		hit
	}

	pub fn put(&self, key: &str, value: i64) {
		self.inner
			.lock()
			.unwrap()
			.insert(key.to_owned(), (Instant::now(), value));
		crate::log_trace!("count cache stored {value} for {key:?}");
	}

	/// Drop every entry. Called by write handlers: a bookmark, tag, or
	/// category mutation can change any filter's count, so coarse
	/// invalidation is the safe choice.
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

/// Pre-serialized JSON responses for the aggregate stats endpoints, keyed by
/// endpoint + pagination, with a longer TTL than the count cache. The stats
/// queries are full-corpus GROUP BY / ORDER BY passes (~1s at 1M rows — see
/// `examples/bench.rs`), so the dashboard rides this cache while a stale
/// count would be far more visible. Same test-isolation story as
/// `CountCache`: it lives in `AppState` and is invalidated alongside it.
#[derive(Debug)]
pub struct StatsCache {
	inner: Mutex<HashMap<String, (Instant, Vec<u8>)>>,
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
				Some((at, body)) if at.elapsed() < self.ttl => Some(body.clone()),
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

	pub fn put(&self, key: &str, body: Vec<u8>) {
		crate::log_trace!("stats cache stored ({} bytes) for {key:?}", body.len());
		self.inner
			.lock()
			.unwrap()
			.insert(key.to_owned(), (Instant::now(), body));
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
