/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Table-driven favicon/thumbnail resolution.
//!
//! The engine is two static tables (`sites::SITE_RULES` for offline,
//! string-only rules; `sites::SITE_FETCHERS` for network-assisted, per-site
//! fetchers) plus a linear scan over them. Adding a site never touches this
//! file: create `core/sites/<site>.rs` with the extractor functions plus a
//! `ROWS` slice, register it in `core/sites/mod.rs`, and you're done —
//! matching, first-match-wins, fallback, and the table-driven tests all
//! pick the new rows up automatically.
//!
//! Matching is *host suffix + optional path prefix*, deliberately not raw
//! substring matching, so a URL like `https://evil.example/...youtube.com/watch...`
//! can never false-match a YouTube rule.
//!
//! The offline table is synchronous and string-only: no network I/O, no
//! per-site error plumbing. A rule either produces a URL or returns `None`
//! and the resolution falls through to the generic `favicon.ico` fallback.
//! The network fetchers are best-effort `Option`s run before the generic
//! scrape (`<link rel=icon>` / `og:image`) in `core::fetch`, target-scoped
//! by `MediaTarget`.
//!
//! # When is this called?
//!
//! `database::bookmarks::insert` runs `media::resolve_favicon(url)` and
//! `media::resolve_thumbnail(url)` when a bookmark is created/updated, and
//! stores the results in the `favicon` / `thumbnail` columns. For any URL
//! matching a registered site fetcher (YouTube today) those two resolve the
//! real icon — a video's channel avatar, a channel's icon, the CDN
//! thumbnail — through the cache-first network pipeline; every other site
//! resolves offline (rule table, then the generic `favicon.ico` fallback).
//! The frontend renders the thumbnail (`.card-thumb`) and the favicon when
//! present.
//!
//! The `fetch` family (`media::fetch_favicon` / `media::fetch_thumbnail`
//! plus their `_fresh` variants) is the cache-first network pipeline that
//! `resolve_*` and the explicit `Fetch` asset mode share: successful
//! network resolutions are reused for 90 days instead of re-scraping on
//! every save. `resolve_*` differs from `Fetch` mode only in scope — the
//! network leg runs for URLs with a matching site fetcher, while `Fetch`
//! mode runs it for every URL.

use super::sites::{SITE_FETCHERS, SITE_RULES, SiteFetcher};
use crate::model::{
	AssetMode, Bookmark, DEFAULT_FAVICON, DEFAULT_THUMBNAIL, NewBookmark, UpdateBookmark,
};

/// What a rule targets. `Favicon` and `Thumbnail` resolve independently:
/// the first matching rule of *that target* wins, then fallbacks apply.
///
/// The split matters because the two have different fallback semantics:
/// every site gets *some* favicon (a rule, else `/favicon.ico`), but only
/// sites with an actual thumbnail rule get a thumbnail at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaTarget {
	Favicon,
	Thumbnail,
}

/// One row of the resolution table: a host suffix, an optional path prefix,
/// the target, and the extractor function that turns a matching URL into a
/// media URL. `examples` doubles as documentation and as the table-driven
/// test's input, so a new row can't land without proof.
///
/// The extractor is a plain `fn` (not a closure) so the whole table can be
/// a `static`. `examples` pairs are `(input_url, expected_output)`; the
/// expected output is `Some(...)` for a URL the extractor should produce,
/// or `None` for a URL that should fall through to the generic fallback.
pub struct SiteRule {
	pub host_suffix: &'static str,
	pub path_prefix: Option<&'static str>,
	pub target: MediaTarget,
	pub extract: fn(&str) -> Option<String>,
	pub examples: &'static [(&'static str, Option<&'static str>)],
}

/// Splits a URL into its scheme and host. Host excludes port, userinfo, and
/// any path/query/fragment. Returns `None` for the host when the input has
/// no recognizable host at all.
///
/// Pipeline (mirrors `shared::extract_domain`, but also keeps the scheme,
/// which the favicon fallback needs to rebuild the URL):
/// 1. strip the scheme (`split_once("://")`);
/// 2. cut at the first `/`, `?`, or `#` to drop path/query/fragment;
/// 3. drop userinfo with `rsplit('@')` — keep only the last `@` segment;
/// 4. drop the port after the first `:`;
/// 5. trim whitespace.
///
/// The scheme is returned even when the host is missing so callers can
/// distinguish "malformed URL" from "scheme-less URL".
///
/// `pub(crate)` so `core::fetch` could reuse it if it ever needs to
/// rebuild URLs from pieces.
pub(crate) fn scheme_and_host(url: &str) -> (Option<&str>, Option<&str>) {
	let (scheme, rest) = match url.split_once("://") {
		Some((s, r)) => (Some(s), r),
		None => (None, url),
	};
	let host_and_rest = rest.split(['/', '?', '#']).next().unwrap_or(rest);
	let host = host_and_rest.rsplit('@').next().unwrap_or(host_and_rest);
	let host = host.split(':').next().unwrap_or(host);
	let host = host.trim();
	if host.is_empty() {
		(scheme, None)
	} else {
		(scheme, Some(host))
	}
}

/// Extracts the path (with leading slash) from a URL, query and fragment
/// stripped. `/` for a bare host.
///
/// Only the path portion is compared against `path_prefix`, so a prefix
/// like `/watch` matches `/watch` and `/watch?v=x` but never a query or
/// fragment — query order therefore can't break a match.
fn path_of(url: &str) -> &str {
	let without_query = url.split(['?', '#']).next().unwrap_or(url);
	match without_query.find("://") {
		Some(idx) => {
			let rest = &without_query[idx + 3..];
			match rest.find('/') {
				Some(slash) => &rest[slash..],
				None => "/",
			}
		}
		None => without_query,
	}
}

/// Host-suffix match: exact, or a dotted subdomain (`www.youtube.com`
/// matches `youtube.com`; `youtube.com.evil.example` does not). Case is
/// ignored — hosts are case-insensitive.
///
/// The `start - 1` boundary check is what enforces "dotted": the suffix
/// may only appear at a label boundary, so `youtube.com.evil.example`
/// (where the char before the suffix would be `.`, not a boundary at the
/// *start*) can't match. Note `url_host.len() > suffix.len()` guarantees
/// `start >= 1`, so `start - 1` never underflows.
fn host_matches(url_host: &str, suffix: &str) -> bool {
	if url_host.eq_ignore_ascii_case(suffix) {
		return true;
	}
	if url_host.len() > suffix.len() {
		let start = url_host.len() - suffix.len();
		if url_host.as_bytes()[start - 1] == b'.' && url_host[start..].eq_ignore_ascii_case(suffix)
		{
			return true;
		}
	}
	false
}

/// First matching rule of the given target wins. `None` when no rule of
/// that target applies — the caller decides what falls back to what.
///
/// The scan is `SITE_RULES` → each inner `ROWS` slice in order; within a
/// slice the *first* rule whose target/host/path all match is used. Because
/// matching is host suffix + path prefix, a URL with no host (`None` here)
/// simply never matches anything.
