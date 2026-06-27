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
		static FALLBACK: OnceLock<PathBuf> = OnceLock::new();
		FALLBACK
			.get_or_init(|| {
				let dir = std::env::temp_dir()
					.join(format!("waypoint-test-cache-{}", std::process::id()));
				let _ = fs::create_dir_all(&dir);
				dir
			})
			.clone()
	} else if let Some(dir) = std::env::var_os("WAYPOINTD_CACHE_DIR") {
		PathBuf::from(dir)
	} else {
		dirs::cache_dir()
			.map(|dir| dir.join("waypoint"))
			.unwrap_or_else(|| std::env::temp_dir().join("waypoint-cache"))
	}
}

/// Full path of the cache file.
pub fn cache_file() -> PathBuf {
	cache_dir().join(FILE_NAME)
}

/// Looks a cached result up. An expired (or absent) entry returns `None`;
/// the stale entry is dropped from memory so a later save can't persist it.
pub fn get(target: MediaTarget, url: &str) -> Option<String> {
	let now = now_millis();
	let mut store = store()
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner());
	let map = store.map_for(target);
	match map.get(url) {
		Some(entry) if entry.expires_at > now => Some(entry.value.clone()),
		_ => {
			map.remove(url);
			None
		}
	}
}

/// Stores a successful fetch result under the default TTL and persists it.
pub fn put(target: MediaTarget, url: &str, value: &str) {
	put_with_ttl(target, url, value, DEFAULT_TTL_SECS);
}

/// Stores a result with an explicit TTL in *seconds* (the default path
/// funnels through here so expiry is testable without a clock); timestamps
/// are stored in milliseconds.
fn put_with_ttl(target: MediaTarget, url: &str, value: &str, ttl_secs: u64) {
	let now = now_millis();
	let mut store = store()
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner());
	store.map_for(target).insert(
		url.to_string(),
		Entry::new(value.to_string(), now, ttl_secs.saturating_mul(1000)),
	);
	if store.len() > MAX_ENTRIES {
		store.purge_expired(now);
		store.trim_oldest(MAX_ENTRIES);
	}
	save(&store);
}

/// Drops every cached entry for a URL (both targets) and persists the
/// change. Used by `update --refresh` before re-resolving.
pub fn evict(url: &str) {
	let mut store = store()
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner());
	let removed = store.favicon.remove(url).is_some() || store.thumbnail.remove(url).is_some();
	if removed {
		save(&store);
	}
}

/// Reads the cache file into memory. A missing file is a fresh store; an
/// unreadable or version-mismatched file is logged and treated as empty.
fn load() -> Store {
	load_from(&cache_file())
}

/// The path-aware half of [`load`], so tests can point at a temp file.
fn load_from(path: &Path) -> Store {
	let bytes = match fs::read(path) {
		Ok(bytes) => bytes,
		Err(_) => return Store::default(),
	};
	match serde_json::from_slice::<FileFormat>(&bytes) {
		Ok(file) if file.version == CACHE_VERSION => {
			let mut store = Store {
				favicon: file.favicon,
				thumbnail: file.thumbnail,
			};
			store.purge_expired(now_millis());
			// A file written before the precise cap (or hand-edited) can
			// arrive over `MAX_ENTRIES`; trim on load too, not just on put.
			store.trim_oldest(MAX_ENTRIES);
			store
		}
		_ => {
			crate::log_warn!(
				"cache: ignoring unreadable or outdated cache file at {}",
				path.display()
			);
			Store::default()
		}
	}
}

/// Atomically writes the store to disk: write a temp file, then rename it
/// over the real file. A crash mid-write leaves the old file intact.
fn save(store: &Store) {
	save_to(store, &cache_file())
}

/// The path-aware half of [`save`], so tests can write to a temp file.
fn save_to(store: &Store, path: &Path) {
	if let Some(parent) = path.parent()
		&& !parent.is_dir()
		&& let Err(err) = fs::create_dir_all(parent)
	{
		crate::log_warn!("cache: cannot create cache dir {}: {err}", parent.display());
		return;
	}
	let json = match serde_json::to_string(&FileFormat::current(store)) {
		Ok(json) => json,
		Err(err) => {
			crate::log_warn!("cache: failed to serialize cache: {err}");
			return;
		}
	};
	let tmp = path.with_extension(format!("{}.{}.tmp", FILE_NAME, std::process::id()));
	if let Err(err) = fs::write(&tmp, &json) {
		crate::log_warn!("cache: cannot write {}: {err}", tmp.display());
		return;
	}
	if let Err(err) = fs::rename(&tmp, path) {
		crate::log_warn!(
			"cache: cannot move {} → {}: {err}",
			tmp.display(),
			path.display()
		);
		let _ = fs::remove_file(&tmp);
	}
}

/// Unix epoch **milliseconds**, the timestamp basis for `Entry` lifetime
/// fields (second resolution could not order writes within the same second).
fn now_millis() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis() as u64)
		.unwrap_or(0)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn tmp_dir(tag: &str) -> PathBuf {
		std::env::temp_dir().join(format!("waypoint-cache-test-{tag}-{}", std::process::id()))
	}

	// --- Store-level logic (no filesystem, no singleton) ---

	#[test]
	fn store_roundtrip_and_evict() {
		let mut store = Store::default();
		store.map_for(MediaTarget::Favicon).insert(
			"https://a.example/".into(),
			Entry::new("https://a.example/favicon.ico".into(), 100, 100),
		);
		store.map_for(MediaTarget::Thumbnail).insert(
			"https://a.example/".into(),
			Entry::new("https://a.example/og.png".into(), 100, 100),
		);

		assert_eq!(store.len(), 2);
		assert_eq!(
			store
				.favicon
				.get("https://a.example/")
				.map(|e| e.value.as_str()),
			Some("https://a.example/favicon.ico")
		);
		assert_eq!(
			store
				.thumbnail
				.get("https://a.example/")
				.map(|e| e.value.as_str()),
			Some("https://a.example/og.png")
		);

		store.favicon.remove("https://a.example/");
		store.thumbnail.remove("https://a.example/");
		assert!(store.is_empty());
	}

	#[test]
	fn purge_expired_drops_only_stale() {
		let mut store = Store::default();
		let now = 1_000;
		store.favicon.insert(
			"fresh".into(),
			Entry::new("https://x/f.ico".into(), now, 60),
		);
		store.favicon.insert(
			"stale".into(),
			Entry::new("https://y/f.ico".into(), now - 120, 60),
		);
		store.thumbnail.insert(
			"stale".into(),
			Entry::new("https://y/og.png".into(), now - 120, 60),
		);

		store.purge_expired(now);

		assert_eq!(store.favicon.len(), 1);
		assert!(store.favicon.contains_key("fresh"));
		assert!(store.thumbnail.is_empty());
	}

	#[test]
	fn trim_oldest_drops_the_oldest() {
		let mut store = Store::default();
		for i in 0..4 {
			store.favicon.insert(
				format!("u{i}"),
				Entry::new(format!("https://u{i}/f.ico"), 100 + i, 1000),
			);
		}

		store.trim_oldest(2);

		// The two newest (created_at 102, 103) survive; the two oldest go.
		assert_eq!(store.favicon.len(), 2);
		assert!(store.favicon.contains_key("u2"));
		assert!(store.favicon.contains_key("u3"));
	}

	// The cap counts each (target, url) pair: a URL's favicon and thumbnail
	// are distinct entries, and entries sharing a `created_at` must not let
	// extra entries slip past the cap (the old timestamp-cutoff bug).
	#[test]
	fn trim_oldest_counts_targets_and_breaks_ties() {
		let mut store = Store::default();
		// Favicon entries all "created" at the same tick.
		for i in 0..4 {
			store.favicon.insert(
				format!("same-{i}"),
				Entry::new(format!("https://u{i}/f.ico"), 500, 1000),
			);
		}
		// The same URL also has a thumbnail, newer by one tick.
		store.thumbnail.insert(
			"same-0".to_string(),
			Entry::new("https://u0/o.png".to_string(), 501, 1000),
		);

		store.trim_oldest(2);

		// The thumbnail (newest tick) + one favicon survive; the favicon of
		// `same-0` is evicted even though its URL survived as a thumbnail.
		assert_eq!(store.len(), 2);
		assert!(store.thumbnail.contains_key("same-0"));
		let favicons: Vec<_> = store.favicon.keys().cloned().collect();
		assert_eq!(favicons.len(), 1);
	}

	// --- File I/O (Store ↔ JSON on a tempdir) ---

	#[test]
	fn save_then_load_roundtrips() {
		let dir = tmp_dir("roundtrip");
		let _ = fs::remove_dir_all(&dir);
		let path = dir.join(FILE_NAME);

		let mut store = Store::default();
		store.favicon.insert(
			"https://a.example/".into(),
			Entry::new(
				"https://a.example/favicon.ico".into(),
				now_millis(),
				1_000_000,
			),
		);
		save_to(&store, &path);
		// The atomic rename lands the temp file's content at `path`.
		let reloaded = load_from(&path);

		assert_eq!(reloaded.favicon.len(), 1);
		assert_eq!(
			reloaded
				.favicon
				.get("https://a.example/")
				.map(|e| e.value.as_str()),
			Some("https://a.example/favicon.ico")
		);
		let _ = fs::remove_dir_all(&dir);
	}

	#[test]
	fn missing_file_loads_empty() {
		let path = std::env::temp_dir().join("waypoint-cache-test-missing.json");
		let _ = fs::remove_file(&path);
		assert!(load_from(&path).is_empty());
	}

	#[test]
	fn corrupt_file_loads_empty() {
		let dir = tmp_dir("corrupt");
		let _ = fs::remove_dir_all(&dir);
		fs::create_dir_all(&dir).unwrap();
		let path = dir.join(FILE_NAME);
		fs::write(&path, "this is not json{{").unwrap();

		assert!(load_from(&path).is_empty());
		let _ = fs::remove_dir_all(&dir);
	}

	#[test]
	fn outdated_version_loads_empty() {
		let dir = tmp_dir("version");
		let _ = fs::remove_dir_all(&dir);
		fs::create_dir_all(&dir).unwrap();
		let path = dir.join(FILE_NAME);
		fs::write(
			&path,
			serde_json::to_string(&FileFormat {
				version: CACHE_VERSION + 1,
				favicon: HashMap::new(),
				thumbnail: HashMap::new(),
			})
			.unwrap(),
		)
		.unwrap();

		assert!(load_from(&path).is_empty());
		let _ = fs::remove_dir_all(&dir);
	}

	// --- The process-wide singleton (URLs stay unique across tests) ---

	#[test]
	fn singleton_get_put_evict_and_expiry() {
		// Miss → put → hit.
		assert_eq!(get(MediaTarget::Favicon, "https://s.example/"), None);
		put(
			MediaTarget::Favicon,
			"https://s.example/",
			"https://s.example/f.ico",
		);
		assert_eq!(
			get(MediaTarget::Favicon, "https://s.example/"),
			Some("https://s.example/f.ico".into())
		);

		// A zero-TTL entry is already expired on write.
		put_with_ttl(
			MediaTarget::Thumbnail,
			"https://s.example/",
			"https://s.example/o.png",
			0,
		);
		assert_eq!(get(MediaTarget::Thumbnail, "https://s.example/"), None);

		// Evict drops the favicon too.
		evict("https://s.example/");
		assert_eq!(get(MediaTarget::Favicon, "https://s.example/"), None);
	}
}
