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
fn path_of_url(url: &str) -> &str {
	let without_scheme = match url.split_once("://") {
		Some((_, rest)) => rest,
		None => url,
	};
	match without_scheme.find('/') {
		Some(idx) => &without_scheme[idx..],
		None => "/",
	}
}

/// Whether `url` is a YouTube channel page (`youtube.com` host with a
/// `/@handle` path). These are the URLs whose real avatar only exists
/// inside the page's `ytInitialData` JSON, so they route to `channel_avatar`
/// before the generic favicon scrape.
///
/// Host matching mirrors the rule table's host-suffix rule: exact or a
/// dotted subdomain (`m.youtube.com` matches), never a sibling domain whose
/// name merely *ends in* the suffix (`youtube.com.evil.example` must not).
pub fn is_channel_url(url: &str) -> bool {
	on_youtube(url).is_some_and(|rest| path_of_url(rest).starts_with("/@"))
}

/// Whether `url` is a YouTube video page (`/watch` or `/shorts/{id}` on a
/// `youtube.com` host). Video pages' media — the video thumbnail and the
/// owner channel's avatar — are handled by `video_thumbnail` and
/// `video_channel_avatar`; the channel-icon fetchers don't match these, so
/// they route through the video fetchers instead.
pub fn is_video_url(url: &str) -> bool {
	on_youtube(url).is_some_and(|rest| {
		let path = path_of_url(rest);
		path.starts_with("/watch") || path.starts_with("/shorts/")
	})
}

/// Whether `url` is any YouTube page the cache-first media pipeline should
/// handle: a channel or a video.
pub fn is_youtube_url(url: &str) -> bool {
	on_youtube(url).is_some()
}

/// Extracts a YouTube channel avatar URL from a channel page's
/// `ytInitialData` JSON blob.
///
/// The avatar lives at a deep, version-fragile path inside the blob:
///
/// ```text
/// header.pageHeaderRenderer.content.pageHeaderViewModel.image
///   .decoratedAvatarViewModel.avatar.avatarViewModel.image.sources
/// ```
///
/// `sources` is ordered smallest-to-largest, so the highest-resolution
/// avatar is the last entry. Every failure — no `ytInitialData` script,
/// unparseable JSON, a missing node on the path, or an empty `sources`
/// array — returns `None`, and the caller falls back to the generic
/// favicon scrape. Pure function over `&str`, no network.
pub fn extract_yt_avatar(html: &str) -> Option<String> {
	let json = YT_INITIAL_DATA_RE.captures(html)?.get(1)?;
	crate::log_trace!(
		"extract_yt_avatar: found ytInitialData blob ({} bytes)",
		json.len()
	);
	let data: serde_json::Value = match serde_json::from_str(json.as_str()) {
		Ok(value) => value,
		Err(e) => {
			crate::log_warn!("extract_yt_avatar: ytInitialData is not valid JSON: {e}");
			return None;
		}
	};
	let sources = data
		.get("header")?
		.get("pageHeaderRenderer")?
		.get("content")?
		.get("pageHeaderViewModel")?
		.get("image")?
		.get("decoratedAvatarViewModel")?
		.get("avatar")?
		.get("avatarViewModel")?
		.get("image")?
		.get("sources")?;
	let avatar = sources.as_array()?.iter().rev().find_map(|source| {
		source
			.get("url")
			.and_then(|url| url.as_str())
			.filter(|url| url.starts_with("http://") || url.starts_with("https://"))
	})?;
	crate::log_trace!("extract_yt_avatar: avatar {avatar:?}");
	Some(avatar.to_string())
}

/// Extracts the channel avatar URL from a video page's `ytInitialData`.
///
/// The watch page's owner block lives at a version-fragile path:
///
/// ```text
/// contents.twoColumnWatchNextResults.results.results.contents[]
///   .videoSecondaryInfoRenderer.owner.videoOwnerRenderer.thumbnail.thumbnails
/// ```
///
/// `contents` is an array of heterogeneous renderer objects; rows that have
/// no `videoSecondaryInfoRenderer` (related videos, ads, ...) are skipped.
/// `thumbnails` is ordered smallest-to-largest, so the highest-resolution
/// avatar is the last entry. Every failure — no blob, unparseable JSON, a
/// missing node, an empty `thumbnails` array — returns `None`. Pure
/// function over `&str`, no network.
pub fn extract_yt_video_owner_avatar(html: &str) -> Option<String> {
	let json = YT_INITIAL_DATA_RE.captures(html)?.get(1)?;
	crate::log_trace!(
		"extract_yt_video_owner_avatar: found ytInitialData blob ({} bytes)",
		json.len()
	);
	let data: serde_json::Value = match serde_json::from_str(json.as_str()) {
		Ok(value) => value,
		Err(e) => {
			crate::log_warn!("extract_yt_video_owner_avatar: ytInitialData is not valid JSON: {e}");
			return None;
		}
	};
	let contents = data
		.get("contents")?
		.get("twoColumnWatchNextResults")?
		.get("results")?
		.get("results")?
		.get("contents")?
		.as_array()?;
	for item in contents {
		let Some(owner) = item
			.get("videoSecondaryInfoRenderer")
			.and_then(|r| r.get("owner"))
			.and_then(|o| o.get("videoOwnerRenderer"))
		else {
			continue;
		};
		let Some(sources) = owner
			.get("thumbnail")
			.and_then(|t| t.get("thumbnails"))
			.and_then(|s| s.as_array())
		else {
			continue;
		};
		if let Some(avatar) = sources.iter().rev().find_map(|thumb| {
			thumb
				.get("url")
				.and_then(|url| url.as_str())
				.filter(|url| url.starts_with("http://") || url.starts_with("https://"))
		}) {
			crate::log_trace!("extract_yt_video_owner_avatar: channel avatar {avatar:?}");
			return Some(avatar.to_string());
		}
	}
	crate::log_trace!("extract_yt_video_owner_avatar: no channel avatar found");
	None
}

/// Fetches a YouTube channel page and extracts its avatar URL.
///
/// Best-effort: `None` on any failure (the caller falls back to the
/// regular favicon scrape). Registered in `super::SITE_FETCHERS` under the
/// `Favicon` target.
pub fn channel_avatar(url: &str) -> Option<String> {
	crate::log_trace!("channel_avatar: fetching {url:?}");
	let html = match fetch_html_limited(url, MAX_YT_BODY_BYTES, Some(YT_UA)) {
		Ok(html) => html,
		Err(e) => {
			crate::log_warn!("channel_avatar: {e} for {url:?}");
			return None;
		}
	};
	extract_yt_avatar(&html)
}

/// Fetches a YouTube video page and extracts its owner channel's avatar —
/// the favicon a video bookmark should carry (its channel's icon, not the
/// generic YouTube favicon).
///
/// Best-effort: `None` on any failure (the caller falls back to the regular
/// favicon scrape). Registered in `super::SITE_FETCHERS` under the
/// `Favicon` target, matching video URLs.
pub fn video_channel_avatar(url: &str) -> Option<String> {
	crate::log_trace!("video_channel_avatar: fetching {url:?}");
	let html = match fetch_html_limited(url, MAX_YT_BODY_BYTES, Some(YT_UA)) {
		Ok(html) => html,
		Err(e) => {
			crate::log_warn!("video_channel_avatar: {e} for {url:?}");
			return None;
		}
	};
	extract_yt_video_owner_avatar(&html)
}

// --- offline rule table (backing the `Auto` asset mode) -----------------

/// Extracts a video id from a `/watch` URL's `v` query parameter. Query
/// order is irrelevant (`?v=x` or `?t=30&v=x` both work).
///
/// The media engine has already verified the path starts with `/watch`,
/// so here we only care about the query string: split off everything after
/// the first `?`, walk the `&`-separated pairs, and take the first `v=...`.
/// A missing `v` (or an empty one) returns `None` → no thumbnail.
