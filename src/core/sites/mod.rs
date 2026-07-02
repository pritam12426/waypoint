/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Site-specific media rules and network fetchers. To add a site:
//!
//! 1. create `sites/<site>.rs` with extractor functions + a `ROWS` slice
//!    (offline rule table) and, when the site's real media needs a live
//!    page fetch, a `matches` predicate + fetch function;
//! 2. register them below (`pub mod <site>;` + one entry in `SITE_RULES`
//!    and/or `SITE_FETCHERS`).
//!
//! That's the whole integration: the resolver in `core::media`, the
//! first-match-wins rule, the fallbacks, and the table-driven tests consume
//! both tables automatically. Adding a site never touches `core::media`,
//! `core::fetch`, or the database layer.
//!
//! # Why a separate module per site
//!
//! Each site's extractors are self-contained — they parse that site's URL
//! shapes (`/watch`, `/shorts`, `/@channel`, ...) into media URLs, and its
//! network fetchers scrape the pages that hide their real media URL in page
//! JSON. Keeping them out of `media.rs`/`fetch.rs` means the engines stay
//! stable, untouched generic scanners and sites are pure additions.

use super::media::{MediaTarget, SiteRule};

pub mod youtube;

/// Master rule table. Order matters only within a target: the first rule of
/// a given `MediaTarget` whose host/path matches wins.
///
/// This is a `&[&[SiteRule]]` (a list of per-site rule slices, not one flat
/// array) purely for readability — each `ROWS` slice reads as its own site's
/// section. The resolver iterates it linearly; there are no priorities or
/// weights, just first-match-wins within a target.
///
/// These rules are offline (no network I/O) and back the `Auto` asset mode:
/// `core::media` runs them at bookmark insert/update time.
pub static SITE_RULES: &[&[SiteRule]] = &[youtube::ROWS];

/// A network-assisted media extractor for a specific kind of URL.
///
/// `target` says whether the fetcher resolves a favicon or a thumbnail;
/// `matches` decides whether the fetcher applies to a bookmark URL; `fetch`
/// fetches the page and extracts the media URL (best-effort `Option`).
/// The first matching fetcher of *that target* that produces a URL wins; a
/// matching fetcher returning `None` (its fetch or extraction failed) falls
/// through to the next one and finally to the generic scrape.
///
/// These back the `Fetch` asset mode: `core::media::fetch_favicon` /
/// `core::media::fetch_thumbnail` run them at bookmark insert/update time,
/// before the generic scrape.
pub struct SiteFetcher {
	/// Human-readable name for log lines, e.g. `"youtube channel"`.
	pub name: &'static str,
	/// Which asset this fetcher resolves: `Favicon` or `Thumbnail`.
	pub target: MediaTarget,
	/// Whether this site's fetcher applies to the given bookmark URL.
	pub matches: fn(&str) -> bool,
	/// Fetches and extracts the media URL for a matching URL.
	pub fetch: fn(&str) -> Option<String>,
}

/// Site-specific network media fetchers, tried in order before the generic
/// scrape (`<link rel=icon>` for favicons, `og:image` for thumbnails).
/// Resolution is target-scoped: `media::fetch_favicon` only consults
/// `Favicon` entries, `media::fetch_thumbnail` only `Thumbnail` entries.
///
/// These back the `Fetch` asset mode **and** the cache-first default
/// resolution (`media::resolve_favicon` / `media::resolve_thumbnail`), which
/// route any URL matching one of these entries through the same pipeline so
/// its columns hold the real icon instead of the generic favicon. Adding a
/// site is one new module (e.g. `youtube.rs`) plus one entry here
/// — nothing else in the codebase changes.
pub static SITE_FETCHERS: &[SiteFetcher] = &[
	SiteFetcher {
		name: "youtube channel",
		target: MediaTarget::Favicon,
		matches: youtube::is_channel_url,
		fetch: youtube::channel_avatar,
	},
	SiteFetcher {
		name: "youtube video channel",
		target: MediaTarget::Favicon,
		matches: youtube::is_video_url,
		fetch: youtube::video_channel_avatar,
	},
	SiteFetcher {
		name: "youtube video thumbnail",
		target: MediaTarget::Thumbnail,
		matches: youtube::is_video_url,
		fetch: youtube::video_thumbnail,
	},
];
