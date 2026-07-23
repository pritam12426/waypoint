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
