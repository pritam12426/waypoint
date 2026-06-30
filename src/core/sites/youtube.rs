/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Site-specific media rules and network fetchers — the reference
//! implementation for how a site module looks.
//!
//! Everything about one site lives in one file:
//!
//! * **Rule table** (`ROWS`): offline, string-only extractors that turn a
//!   matching URL into a media URL at save time with no network I/O. These
//!   back the `Auto` asset mode.
//! * **Network fetchers**: extractors that need a live page fetch — a
//!   channel's avatar only exists inside its `ytInitialData` JSON, which a
//!   `<link rel=icon>` scrape can never find. These back the `Fetch` asset
//!   mode and are wired in via `super::SITE_FETCHERS`.
//!
//! The extractors are plain `fn(&str) -> Option<String>`: they get the full
//! bookmark URL and either produce a media URL or return `None` to fall
//! through. The `examples` on each rule are both documentation and the
//! inputs the table-driven test in `core::media` runs, so any change here
//! is verified automatically.
//!
//! The thumbnails use YouTube's `i.ytimg.com` CDN — the same image the
//! official player embeds. There's no network call at resolution time for
//! the rule table; the URL is just stored in the `thumbnail` column and the
//! browser fetches it when rendering. The avatar fetchers are the
//! exceptions: a channel page's avatar and a video page's owner-avatar both
//! live inside the page's `ytInitialData` JSON, which a `<link rel=icon>`
//! scrape can never find. They need a live page fetch (multi-megabyte body
//! budget + desktop Chrome `User-Agent`, see `channel_avatar` /
//! `video_channel_avatar`).

use std::sync::LazyLock;

use super::super::fetch::fetch_html_limited;
use super::super::media::{MediaTarget, SiteRule};

// --- network fetch (backing the `Fetch` asset mode) ---------------------

/// Body budget for YouTube page fetches. Real channel and watch pages are
/// routinely well over a megabyte, so the generic 512 KB cap would fail
/// before the `ytInitialData` script is even reached.
const MAX_YT_BODY_BYTES: u64 = 4 * 1024 * 1024;

/// Desktop Chrome `User-Agent` for YouTube page fetches. YouTube serves a
/// consent/bot page to unknown agents that omits `ytInitialData` — the
/// avatar only exists on the page a real browser would get.
const YT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
	AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// The `ytInitialData = { ... };` JSON blob YouTube inlines in a page's
/// first script tag (channel pages, watch pages, ...). Dot-all and
/// non-greedy, so it stops at the first `});</script>` boundary.
static YT_INITIAL_DATA_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?s)ytInitialData\s*=\s*(\{.*?\});</script>")
		.expect("static YT_INITIAL_DATA_RE is valid")
});

/// Host-suffix check shared by the YouTube URL predicates: the URL must be
/// `http(s)://youtube.com` or a dotted subdomain (`m.youtube.com` matches,
/// `youtube.com.evil.example` does not — same boundary rule as the rule
/// table), case-insensitive. Returns the rest of the URL (host + path +
/// query) on a match.
fn on_youtube(url: &str) -> Option<&str> {
	let without_scheme = match url.split_once("://") {
		Some(("http" | "https", rest)) => rest,
		_ => return None,
	};
	let host = without_scheme
		.split(['/', '?', '#'])
		.next()
		.unwrap_or(without_scheme)
		.rsplit('@')
		.next()
		.unwrap_or(without_scheme);
	let host = host.split(':').next().unwrap_or(host).trim();
	if !host.eq_ignore_ascii_case("youtube.com")
		&& !host.to_ascii_lowercase().ends_with(".youtube.com")
	{
		return None;
	}
	Some(without_scheme)
}

/// The path portion (leading `/` onward) of a URL, query and fragment kept
/// — the `starts_with` checks below don't need them stripped.
