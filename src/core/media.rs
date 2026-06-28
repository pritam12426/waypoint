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
fn first_match(url: &str, target: MediaTarget) -> Option<String> {
	let (_scheme, host) = scheme_and_host(url);
	let host = host?;
	let path = path_of(url);
	for rules in SITE_RULES {
		for rule in *rules {
			if rule.target == target
				&& host_matches(host, rule.host_suffix)
				&& rule
					.path_prefix
					.is_none_or(|prefix| path.starts_with(prefix))
			{
				let extracted = (rule.extract)(url);
				crate::log_trace!(
					"{target:?} rule host={:?} prefix={:?} matched {url:?} → {extracted:?}",
					rule.host_suffix,
					rule.path_prefix,
				);
				return extracted;
			}
		}
	}
	crate::log_trace!("no {target:?} rule matched {url:?}");
	None
}

/// Generic last-resort favicon for any site: `{scheme}://{host}/favicon.ico`.
/// Only reached when no favicon rule matched.
///
/// Falls back to `https` when the URL had no explicit scheme, so even a
/// scheme-less URL gets a sensible favicon. Unlike `scheme_and_host`, the
/// authority keeps its `:port` — a `127.0.0.1:8080` dev server's generic
/// favicon must point back at the same port, not drop it.
///
/// `pub(crate)` so `core::fetch` could use it as the last-resort for a
/// page with no `<link rel=icon>`.
pub(crate) fn fallback_favicon(url: &str) -> Option<String> {
	let (scheme, rest) = match url.split_once("://") {
		Some((s, r)) => (Some(s), r),
		None => (None, url),
	};
	// Authority = the first `host[:port]` run after the scheme, userinfo
	// (before the last `@`) stripped.
	let authority_and_rest = rest.split(['/', '?', '#']).next().unwrap_or(rest);
	let authority = authority_and_rest
		.rsplit('@')
		.next()
		.unwrap_or(authority_and_rest);
	let authority = authority.trim();
	if authority.is_empty() {
		crate::log_warn!(
			"cannot derive a default favicon for {url:?}: the URL has no recognizable host"
		);
		return None;
	}
	Some(format!(
		"{}://{authority}/favicon.ico",
		scheme.unwrap_or("https")
	))
}

/// Resolves a favicon URL: a rule may override it (a channel-icon extractor,
/// a known CDN path, ...), otherwise the domain's `/favicon.ico` is used.
pub fn favicon(url: &str) -> Option<String> {
	crate::log_trace!("resolving favicon for {url:?}");
	let resolved = first_match(url, MediaTarget::Favicon).or_else(|| fallback_favicon(url));
	crate::log_trace!("favicon resolved for {url:?} → {resolved:?}");
	resolved
}

/// Resolves *only* the generic default favicon: `{scheme}://{host}/favicon.ico`,
/// never a site-specific/custom rule. This is what `--no-custom-favicon`
/// stores — a bookmark that must never pick up a custom channel icon or
/// vendor path, just the plain domain favicon.
pub fn default_favicon(url: &str) -> Option<String> {
	crate::log_trace!(
		"resolving *generic* favicon for {url:?} (--no-custom-favicon path, rules skipped)"
	);
	let resolved = fallback_favicon(url);
	crate::log_trace!("generic favicon resolved for {url:?} → {resolved:?}");
	resolved
}

/// Resolves a thumbnail URL. Stays `None` for everything without a matching
/// thumbnail rule — most bookmarks simply have no thumbnail.
pub fn thumbnail(url: &str) -> Option<String> {
	crate::log_trace!("resolving thumbnail for {url:?}");
	let resolved = first_match(url, MediaTarget::Thumbnail);
	crate::log_trace!("thumbnail resolved for {url:?} → {resolved:?}");
	resolved
}

/// The default (no-mode) favicon resolution. Cache-first **for any URL that
/// matches a registered site fetcher** (`sites::SITE_FETCHERS`, `Favicon`
/// target) — for those sites the real icon is fetched once and reused from
/// the media cache for 90 days, so a YouTube channel/video bookmark's
/// `favicon` column holds its channel avatar instead of the generic
/// `youtube.com/favicon.ico`. Every other URL resolves offline through the
/// rule table ([`favicon`]) — no network, no cache.
///
/// General by design: registering a new site fetcher (one module + one
/// `SITE_FETCHERS` entry) automatically opts that site into cache-first
/// default resolution; nothing here changes.
///
/// On a cache miss the site fetcher fetches the page, the resulting URL is
/// cached (90-day TTL), and a failed fetch degrades to the offline
/// [`favicon`] fallback. This is the pipeline the `Fetch` asset mode runs
/// for every URL, scoped here to sites with a registered fetcher so the
/// default save stays offline for everything else.
pub fn resolve_favicon(url: &str) -> Option<String> {
	crate::log_trace!("resolve_favicon: smart resolution for {url:?}");
	if site_fetcher_matches(SITE_FETCHERS, url, MediaTarget::Favicon) {
		fetch_favicon(url)
	} else {
		favicon(url)
	}
}

/// The default (no-mode) thumbnail resolution. Cache-first for any URL that
/// matches a registered site fetcher (`sites::SITE_FETCHERS`,
/// `Thumbnail` target) — e.g. a YouTube video bookmark's `thumbnail` column
/// holds the CDN thumbnail, cached after the first fetch. URLs without a
/// matching fetcher resolve offline through the rule table ([`thumbnail`]).
pub fn resolve_thumbnail(url: &str) -> Option<String> {
	crate::log_trace!("resolve_thumbnail: smart resolution for {url:?}");
	if site_fetcher_matches(SITE_FETCHERS, url, MediaTarget::Thumbnail) {
		fetch_thumbnail(url)
	} else {
		thumbnail(url)
	}
}

/// Whether any site fetcher with `target` would apply to `url` — i.e. its
/// `matches(url)` is true. Used to decide if the cache-first network
/// pipeline is eligible for a URL without actually running a fetch.
fn site_fetcher_matches(fetchers: &[SiteFetcher], url: &str, target: MediaTarget) -> bool {
	fetchers
		.iter()
		.any(|fetcher| fetcher.target == target && (fetcher.matches)(url))
}

/// Runs the site-specific network fetchers for one target, in table order.
/// The first fetcher whose `target` matches, whose `matches(url)` is true,
/// and whose `fetch(url)` produces a URL wins; a matching fetcher returning
/// `None` (its fetch or extraction failed) falls through to the next one.
///
/// Extracted from `fetch_favicon`/`fetch_thumbnail` so the target-scoping
/// and fall-through rules can be unit-tested against synthetic fetchers
/// with no network.
