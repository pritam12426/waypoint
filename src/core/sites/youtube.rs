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
fn watch_thumbnail(url: &str) -> Option<String> {
	let query = url.split('?').nth(1)?;
	for pair in query.split('&') {
		let (key, value) = pair.split_once('=')?;
		if key == "v" && !value.is_empty() {
			let thumb = format!("https://i.ytimg.com/vi/{value}/hqdefault.jpg");
			crate::log_trace!("youtube /watch rule: video id {value:?} from {url:?} → {thumb}");
			return Some(thumb);
		}
	}
	crate::log_trace!("youtube /watch rule: no v= param in {url:?}; no thumbnail");
	None
}

/// Extracts a video id from a `/shorts/{id}` URL (the first path segment
/// after the `/shorts/` marker).
///
/// Strips any query/fragment first, then takes the token following the
/// `/shorts/` literal. The `split('/').next()` caps the id at one path
/// segment, so `/shorts/abc/extra` still yields `abc`.
fn shorts_thumbnail(url: &str) -> Option<String> {
	let path = url.split(['?', '#']).next().unwrap_or(url);
	let id = path
		.split_once("/shorts/")?
		.1
		.split('/')
		.next()
		.unwrap_or("");
	if id.is_empty() {
		crate::log_trace!("youtube /shorts rule: empty id in {url:?}; no thumbnail");
		return None;
	}
	let thumb = format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg");
	crate::log_trace!("youtube /shorts rule: video id {id:?} from {url:?} → {thumb}");
	Some(thumb)
}

/// Resolves a video URL's CDN thumbnail (`i.ytimg.com/vi/{id}/hqdefault.jpg`)
/// — deterministic, no network. Dispatches on path: `/shorts/` → the shorts
/// rule, otherwise `/watch` (which needs the `v` query param).
///
/// The offline rule table already produces the same URL for `Auto` mode;
/// this is registered in `super::SITE_FETCHERS` under the `Thumbnail`
/// target so the *cache-first* pipeline (`fetch_thumbnail`) serves it from
/// the media cache instead of scraping the page.
pub fn video_thumbnail(url: &str) -> Option<String> {
	if path_of_url(url).starts_with("/shorts/") {
		shorts_thumbnail(url)
	} else {
		watch_thumbnail(url)
	}
}

/// Channel pages have no extractable thumbnail; returning `None` sends the
/// favicon resolution on to the generic domain fallback.
///
/// The channel *avatar* (fetch mode) is handled by `channel_avatar` above;
/// the offline `Auto` mode keeps the generic `youtube.com/favicon.ico`
/// since deriving the avatar needs a live page fetch.
fn channel_icon(url: &str) -> Option<String> {
	crate::log_trace!(
		"youtube channel rule matched {url:?}; offline mode falls back to the domain favicon (avatar needs a live fetch)"
	);
	None
}

/// YouTube rules:
///
/// * `/watch?v=...`      → video thumbnail
/// * `/shorts/{id}`      → video thumbnail
/// * `/@channel`         → favicon (offline: falls through to youtube.com/favicon.ico)
///
/// Note the two thumbnail rules share `host_suffix: "youtube.com"` — they
/// differ only by `path_prefix`, and the engine picks the *first* matching
/// rule for the `Thumbnail` target, so `/watch` before `/shorts/` ordering
/// here is cosmetic (their paths can never both match).
pub static ROWS: &[SiteRule] = &[
	SiteRule {
		host_suffix: "youtube.com",
		path_prefix: Some("/watch"),
		target: MediaTarget::Thumbnail,
		extract: watch_thumbnail,
		examples: &[
			(
				"https://www.youtube.com/watch?v=dQw4w9WgXcQ",
				Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"),
			),
			(
				"https://youtube.com/watch?t=30&v=AbCdEf123",
				Some("https://i.ytimg.com/vi/AbCdEf123/hqdefault.jpg"),
			),
		],
	},
	SiteRule {
		host_suffix: "youtube.com",
		path_prefix: Some("/shorts/"),
		target: MediaTarget::Thumbnail,
		extract: shorts_thumbnail,
		examples: &[(
			"https://www.youtube.com/shorts/AbCdEf123",
			Some("https://i.ytimg.com/vi/AbCdEf123/hqdefault.jpg"),
		)],
	},
	SiteRule {
		host_suffix: "youtube.com",
		path_prefix: Some("/@"),
		target: MediaTarget::Favicon,
		extract: channel_icon,
		examples: &[("https://www.youtube.com/@SomeChannel", None)],
	},
];

#[cfg(test)]
mod tests {
	use super::*;

	// --- URL matching ----------------------------------------------------

	#[test]
	fn channel_url_shapes() {
		assert!(is_channel_url("https://www.youtube.com/@SomeChannel"));
		assert!(is_channel_url("https://youtube.com/@a"));
		assert!(is_channel_url("https://m.youtube.com/@a/videos"));
		assert!(!is_channel_url("https://www.youtube.com/watch?v=x"));
		assert!(!is_channel_url("https://www.youtube.com/shorts/x"));
		assert!(!is_channel_url("https://www.youtube.com/channel/UC123"));
		assert!(!is_channel_url("https://youtube.com.evil.example/@a"));
	}

	// --- ytInitialData extraction (no network) ---------------------------

	#[test]
	fn yt_avatar_plain() {
		let html = r#"<html><head><script>var ytInitialData = {"header":{"pageHeaderRenderer":{"content":{"pageHeaderViewModel":{"image":{"decoratedAvatarViewModel":{"avatar":{"avatarViewModel":{"image":{"sources":[{"url":"https://yt3.googleusercontent.com/small","width":48,"height":48},{"url":"https://yt3.googleusercontent.com/large","width":900,"height":900}]}}}}}}}}}};</script></head></html>"#;
		assert_eq!(
			extract_yt_avatar(html).as_deref(),
			Some("https://yt3.googleusercontent.com/large")
		);
	}

	// `sources` is ordered smallest-to-largest; the highest-resolution
	// avatar is the last entry. The page is built with `serde_json`'s
	// pretty-printer so whitespace-heavy (real-world) pages are exercised.
	#[test]
	fn yt_avatar_whitespace_and_ordering() {
		let data = serde_json::json!({
			"header": {
				"pageHeaderRenderer": {
					"content": {
						"pageHeaderViewModel": {
							"image": {
								"decoratedAvatarViewModel": {
									"avatar": {
										"avatarViewModel": {
											"image": {
												"sources": [
													{ "url": "https://yt3.googleusercontent.com/small" },
													{ "url": "https://yt3.googleusercontent.com/medium" },
													{ "url": "https://yt3.googleusercontent.com/large" }
												]
											}
										}
									}
								}
							}
						}
					}
				}
			}
		});
		let html = format!(
			"<script>var ytInitialData = {};</script>",
			serde_json::to_string_pretty(&data).unwrap()
		);
		assert_eq!(
			extract_yt_avatar(&html).as_deref(),
			Some("https://yt3.googleusercontent.com/large")
		);
	}

	// No `ytInitialData` script on the page → no avatar.
	#[test]
	fn yt_avatar_missing_script() {
		assert_eq!(
			extract_yt_avatar("<html>no ytInitialData here</html>"),
			None
		);
	}

	// A captured blob that is not valid JSON degrades to `None`, never a
	// panic.
	#[test]
	fn yt_avatar_bad_json() {
		let html = r#"<script>var ytInitialData = {not valid json} ;</script>"#;
		assert_eq!(extract_yt_avatar(html), None);
	}

	// The blob parses but the deep avatar path is missing → `None`.
	#[test]
	fn yt_avatar_missing_path() {
		let html = r#"<script>var ytInitialData = {"header":{"other":1}};</script>"#;
		assert_eq!(extract_yt_avatar(html), None);
	}

	// An empty `sources` array has nothing to take.
	#[test]
	fn yt_avatar_empty_sources() {
		let html = r#"<script>var ytInitialData = {"header":{"pageHeaderRenderer":{"content":{"pageHeaderViewModel":{"image":{"decoratedAvatarViewModel":{"avatar":{"avatarViewModel":{"image":{"sources":[]}}}}}}}}}};</script>"#;
		assert_eq!(extract_yt_avatar(html), None);
	}

	// --- video URL predicates -------------------------------------------

	#[test]
	fn video_url_shapes() {
		assert!(is_video_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
		assert!(is_video_url("https://youtube.com/watch?t=30&v=AbCdEf123"));
		assert!(is_video_url("https://m.youtube.com/shorts/AbCdEf123"));
		assert!(!is_video_url("https://www.youtube.com/@SomeChannel"));
		assert!(!is_video_url("https://www.youtube.com/"));
		assert!(!is_video_url("https://www.youtube.com/channel/UC123"));
		assert!(!is_video_url("https://youtube.com.evil.example/watch?v=x"));
	}

	#[test]
	fn youtube_url_shapes() {
		assert!(is_youtube_url("https://www.youtube.com/@SomeChannel"));
		assert!(is_youtube_url("https://youtube.com/watch?v=x"));
		assert!(is_youtube_url("https://m.youtube.com/shorts/x"));
		assert!(!is_youtube_url("https://example.com/watch?v=x"));
		assert!(!is_youtube_url("youtube.com.evil.example/@a"));
		assert!(!is_youtube_url("ftp://youtube.com/@a"));
	}

	// --- video thumbnail (deterministic, no network) ---------------------

	#[test]
	fn video_thumbnail_urls() {
		assert_eq!(
			video_thumbnail("https://www.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(),
			Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg")
		);
		assert_eq!(
			video_thumbnail("https://www.youtube.com/watch?t=30&v=AbCdEf123").as_deref(),
			Some("https://i.ytimg.com/vi/AbCdEf123/hqdefault.jpg")
		);
		assert_eq!(
			video_thumbnail("https://www.youtube.com/shorts/AbCdEf123").as_deref(),
			Some("https://i.ytimg.com/vi/AbCdEf123/hqdefault.jpg")
		);
		// A `/watch` URL without a `v` param has no thumbnail.
		assert_eq!(video_thumbnail("https://www.youtube.com/watch"), None);
	}

	// --- ytInitialData owner-avatar extraction (no network) --------------

	#[test]
	fn yt_video_owner_avatar_plain() {
		let html = r#"<html><body><script>var ytInitialData = {"contents":{"twoColumnWatchNextResults":{"results":{"results":{"contents":[{"videoSecondaryInfoRenderer":{"owner":{"videoOwnerRenderer":{"thumbnail":{"thumbnails":[{"url":"https://yt3.googleusercontent.com/small","width":48,"height":48},{"url":"https://yt3.googleusercontent.com/large","width":900,"height":900}]}}}}}]}}}}};</script></body></html>"#;
		// `thumbnails` is ordered smallest-to-largest; the last entry wins.
		assert_eq!(
			extract_yt_video_owner_avatar(html).as_deref(),
			Some("https://yt3.googleusercontent.com/large")
		);
	}

	// A `contents` row without `videoSecondaryInfoRenderer` (a related
	// video, an ad, ...) must be skipped, not treated as the answer — the
	// owner row comes later.
	#[test]
	fn yt_video_owner_avatar_skips_non_owner_rows() {
		let html = r#"<script>var ytInitialData = {"contents":{"twoColumnWatchNextResults":{"results":{"results":{"contents":[{"videoPrimaryInfoRenderer":{"title":{"runs":[{"text":"a"}]}}},{"videoSecondaryInfoRenderer":{"owner":{"videoOwnerRenderer":{"thumbnail":{"thumbnails":[{"url":"https://yt3.googleusercontent.com/owner-avatar"}]}}}}}]}}}}};</script>"#;
		assert_eq!(
			extract_yt_video_owner_avatar(html).as_deref(),
			Some("https://yt3.googleusercontent.com/owner-avatar")
		);
	}

	// A page without `ytInitialData`, a blob that isn't valid JSON, a
	// missing owner path, or an empty `thumbnails` array all degrade to
	// `None`.
	#[test]
	fn yt_video_owner_avatar_missing() {
		assert_eq!(extract_yt_video_owner_avatar("<html>no data</html>"), None);
		let bad_json = r#"<script>var ytInitialData = {not valid};</script>"#;
		assert_eq!(extract_yt_video_owner_avatar(bad_json), None);
		let missing_path = r#"<script>var ytInitialData = {"contents":{"other":1}};</script>"#;
		assert_eq!(extract_yt_video_owner_avatar(missing_path), None);
		let empty_sources = r#"<script>var ytInitialData = {"contents":{"twoColumnWatchNextResults":{"results":{"results":{"contents":[{"videoSecondaryInfoRenderer":{"owner":{"videoOwnerRenderer":{"thumbnail":{"thumbnails":[]}}}}}]}}}}};</script>"#;
		assert_eq!(extract_yt_video_owner_avatar(empty_sources), None);
	}
}
