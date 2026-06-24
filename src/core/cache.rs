/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Persistent cache of fetched-media results.
//!
//! `media::fetch_favicon` / `media::fetch_thumbnail` hit the network on
//! every call, and bookmark saves (plus URL changes) re-fetch the same
//! pages over and over. Their *successful* results are cached on disk,
//! keyed by the bookmark URL + `MediaTarget`, so a page's favicon and
//! thumbnail are only ever fetched once per TTL.
//!
//! # Rules
//!
//! * Only successful network results are cached. A `None` (page down, no
//!   icon link, ...) is never stored, so failures are retried next time.
//! * The offline rule-table fallback (`media::favicon` / `media::thumbnail`)
//!   is *not* cached — it's free, and this keeps rule-table edits instantly
//!   visible instead of pinned by a stale entry.
//! * Entries expire after `DEFAULT_TTL_SECS` (90 days). `update --refresh`
//!   bypasses the cache for one bookmark via the `media::fetch_*_fresh`
//!   entry points (which still rewrite the cache with the fresh result).
//! * The link checker (`core::checker`) does not use this cache; probe
//!   results are deliberately live.
//!
//! # Layout
//!
//! The file lives at `<cache_dir>/waypoint/media-cache.json`, where
//! `cache_dir` is `$WAYPOINTD_CACHE_DIR` if set, else the platform cache
//! directory (`dirs::cache_dir`: Linux `~/.cache`, macOS
//! `~/Library/Caches`, Windows `%LOCALAPPDATA%`), else the temp dir.
//!
//! ```json
//! { "version": 2,
//!   "favicon":   { "<url>": { "value": "...", "created_at": 123, "expires_at": 456 } },
//!   "thumbnail": { "<url>": { ... } } }
//! ```
//!
//! Writes are atomic (temp file + rename) so a crash can't corrupt the
//! file; a corrupt or version-mismatched file is logged and treated as
//! empty. The store is capped at `MAX_ENTRIES` across both targets
//! (oldest dropped) to bound disk use.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::media::MediaTarget;

/// Cache format version; bumping it invalidates every old file.
pub const CACHE_VERSION: u32 = 2;

/// How long a successful fetch result stays valid before being re-fetched.
pub const DEFAULT_TTL_SECS: u64 = 90 * 24 * 60 * 60;

/// Upper bound on cached entries across both targets; the oldest are
/// dropped past this so a busy collection can't grow the file without end.
const MAX_ENTRIES: usize = 10_000;

const FILE_NAME: &str = "media-cache.json";

/// One cached result: the resolved URL plus its lifetime. Both timestamps
/// are Unix epoch **milliseconds** — second resolution let distinct writes
/// inside one second share a `created_at`, which the cap-eviction logic
/// then couldn't order (and a whole flood could ride the same cutoff past
/// `MAX_ENTRIES`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
	value: String,
	created_at: u64,
	expires_at: u64,
}

impl Entry {
	fn new(value: String, now_ms: u64, ttl_ms: u64) -> Self {
		Self {
			value,
			created_at: now_ms,
			expires_at: now_ms.saturating_add(ttl_ms),
		}
	}
}

/// The on-disk JSON shape: a version tag plus one map per target.
#[derive(Debug, Serialize, Deserialize)]
struct FileFormat {
	version: u32,
	#[serde(default)]
	favicon: HashMap<String, Entry>,
	#[serde(default)]
	thumbnail: HashMap<String, Entry>,
}

impl FileFormat {
	fn current(store: &Store) -> Self {
		Self {
			version: CACHE_VERSION,
			favicon: store.favicon.clone(),
			thumbnail: store.thumbnail.clone(),
		}
	}
}

/// In-memory cache: URL → entry, per target.
#[derive(Debug, Default)]
struct Store {
	favicon: HashMap<String, Entry>,
	thumbnail: HashMap<String, Entry>,
}

impl Store {
	fn map_for(&mut self, target: MediaTarget) -> &mut HashMap<String, Entry> {
		match target {
			MediaTarget::Favicon => &mut self.favicon,
			MediaTarget::Thumbnail => &mut self.thumbnail,
		}
	}

	fn len(&self) -> usize {
		self.favicon.len() + self.thumbnail.len()
	}

	#[cfg(test)]
	fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// Drops every entry whose TTL has passed.
	fn purge_expired(&mut self, now_ms: u64) {
		self.favicon.retain(|_, entry| entry.expires_at > now_ms);
		self.thumbnail.retain(|_, entry| entry.expires_at > now_ms);
	}

	/// Caps the store by keeping the `cap` *newest* entries (by `created_at`)
	/// across both targets. Each entry is judged individually, so ties on
	/// `created_at` cannot smuggle extra entries past the cap the way a
	/// timestamp cutoff did.
	fn trim_oldest(&mut self, cap: usize) {
		if self.len() <= cap {
			return;
		}
		let mut newest: Vec<(u64, MediaTarget, String)> = self
			.favicon
			.iter()
			.map(|(key, entry)| (entry.created_at, MediaTarget::Favicon, key.clone()))
			.chain(
				self.thumbnail
					.iter()
					.map(|(key, entry)| (entry.created_at, MediaTarget::Thumbnail, key.clone())),
			)
			.collect();
		// Newest first; `sort_unstable_by_key` is not used because the
		// comparator must be a total order over `(created_at, key)` to keep
		// ties deterministic.
		newest.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| b.2.cmp(&a.2)));
		// Keyed by target too: one URL legitimately has both a favicon and a
		// thumbnail entry, and they must count (and be evicted) separately.
		let keep: std::collections::HashSet<(MediaTarget, String)> = newest
			.iter()
			.take(cap)
			.map(|(_, target, key)| (*target, key.clone()))
			.collect();
		self.favicon
			.retain(|key, _| keep.contains(&(MediaTarget::Favicon, key.clone())));
		self.thumbnail
			.retain(|key, _| keep.contains(&(MediaTarget::Thumbnail, key.clone())));
	}
}

/// The process-wide store, loaded lazily from disk on first use. Wrapped in
/// a `Mutex` (same discipline as the DB connection); entries are small and
/// I/O is infrequent, so contention is a non-issue.
static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> &'static Mutex<Store> {
	STORE.get_or_init(|| Mutex::new(load()))
}

/// Directory the cache lives in: under tests, a per-process temp dir so a
/// test run can't read or pollute a real user cache; otherwise
/// `$WAYPOINTD_CACHE_DIR` wins, then the platform cache directory, then the
/// temp dir.
pub fn cache_dir() -> PathBuf {
	if cfg!(test) {
