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
/// The URL's scheme and host. The host comes from `shared::host_of` (scheme
/// stripped, path/query/fragment, userinfo and port dropped, trimmed) so the
/// media pipeline and `shared::extract_domain` share one splitter; the
/// scheme is kept because the favicon fallback needs it to rebuild the URL.
///
/// The scheme is returned even when the host is missing so callers can
/// distinguish "malformed URL" from "scheme-less URL".
///
/// `pub(crate)` so `core::fetch` could reuse it if it ever needs to
/// rebuild URLs from pieces.
pub(crate) fn scheme_and_host(url: &str) -> (Option<&str>, Option<&str>) {
	let scheme = url.split_once("://").map(|(s, _)| s);
	(scheme, crate::shared::host_of(url))
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
/// `pub(crate)` so site matchers outside the rule table (`core::sites`)
/// reuse the same host-suffix rule.
pub(crate) fn host_matches(url_host: &str, suffix: &str) -> bool {
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
fn run_site_fetchers(fetchers: &[SiteFetcher], url: &str, target: MediaTarget) -> Option<String> {
	for fetcher in fetchers {
		if fetcher.target != target {
			continue;
		}
		if (fetcher.matches)(url) {
			if let Some(found) = (fetcher.fetch)(url) {
				crate::log_trace!(
					"media: {} ({target:?}) matched {url:?} → {found:?}",
					fetcher.name
				);
				return Some(found);
			}
			crate::log_trace!(
				"media: {} ({target:?}) matched {url:?} but produced nothing; falling through",
				fetcher.name
			);
		}
	}
	None
}

/// Network-assisted favicon resolution: the server fetches the page at save
/// time, scrapes its `<link rel=icon>`, and stores the discovered absolute
/// URL. Best-effort by design — when the fetch fails or the page has no
/// icon link, this degrades to the plain table-driven resolution (which
/// itself never fails for a URL with a host), so the stored favicon is
/// always *something*.
///
/// Site-specific network fetchers (`sites::SITE_FETCHERS`, target
/// `Favicon`) run first — e.g. a YouTube channel's real avatar lives inside
/// its `ytInitialData` JSON, which a generic `<link rel=icon>` scrape can
/// never find (the page's icon link is the *generic* YouTube favicon). A
/// matching fetcher's URL wins; otherwise the generic scrape runs, then the
/// rule table.
///
/// Successful network results are cached (see `core::cache`) so repeated
/// saves of the same URL don't re-fetch; the offline rule-table fallback is
/// never cached. `fetch_favicon_fresh` skips the cache read for `--refresh`.
pub fn fetch_favicon(url: &str) -> Option<String> {
	fetch_favicon_inner(url, false)
}

/// Like [`fetch_favicon`] but ignores any cached result and re-fetches now;
/// the fresh result still rewrites the cache entry. Backs `update --refresh`.
pub fn fetch_favicon_fresh(url: &str) -> Option<String> {
	fetch_favicon_inner(url, true)
}

fn fetch_favicon_inner(url: &str, bypass: bool) -> Option<String> {
	crate::log_trace!("fetch_favicon: scraping favicon for {url:?}");
	if !bypass && let Some(hit) = super::cache::get(MediaTarget::Favicon, url) {
		crate::log_trace!("fetch_favicon: cache hit for {url:?} → {hit:?}");
		return Some(hit);
	}
	if let Some(found) = run_site_fetchers(SITE_FETCHERS, url, MediaTarget::Favicon) {
		super::cache::put(MediaTarget::Favicon, url, &found);
		return Some(found);
	}
	// Only genuine network results are cached; the rule-table fallback is
	// free and must not pin a stale result into the cache.
	match super::fetch::fetch_favicon(url) {
		Some(found) => {
			super::cache::put(MediaTarget::Favicon, url, &found);
			crate::log_trace!("fetch_favicon resolved for {url:?} → {found:?}");
			Some(found)
		}
		None => {
			crate::log_trace!(
				"fetch_favicon: no icon scraped for {url:?}, falling back to rule table"
			);
			favicon(url)
		}
	}
}

/// Network-assisted thumbnail resolution: scrapes the page's `og:image`
/// (falling back to `twitter:image`) and stores the discovered absolute
/// URL. Unlike `fetch_favicon` there is no guaranteed fallback — most
/// pages have no social image, so this stays `None` when the fetch fails
/// or the page simply has no `og:image`.
///
/// Site-specific network fetchers (`sites::SITE_FETCHERS`, target
/// `Thumbnail`) run first when a site's real thumbnail hides in page JSON
/// that a generic `og:image` scrape can't reach. A matching fetcher's URL
/// wins; otherwise the generic scrape runs, then the rule table.
///
/// Successful network results are cached (see `core::cache`); `None` and
/// the offline rule-table fallback are not. `fetch_thumbnail_fresh` skips
/// the cache read for `--refresh`.
pub fn fetch_thumbnail(url: &str) -> Option<String> {
	fetch_thumbnail_inner(url, false)
}

/// Like [`fetch_thumbnail`] but ignores any cached result and re-fetches
/// now; the fresh result still rewrites the cache entry. Backs
/// `update --refresh`.
pub fn fetch_thumbnail_fresh(url: &str) -> Option<String> {
	fetch_thumbnail_inner(url, true)
}

fn fetch_thumbnail_inner(url: &str, bypass: bool) -> Option<String> {
	crate::log_trace!("fetch_thumbnail: scraping og:image for {url:?}");
	if !bypass && let Some(hit) = super::cache::get(MediaTarget::Thumbnail, url) {
		crate::log_trace!("fetch_thumbnail: cache hit for {url:?} → {hit:?}");
		return Some(hit);
	}
	if let Some(found) = run_site_fetchers(SITE_FETCHERS, url, MediaTarget::Thumbnail) {
		super::cache::put(MediaTarget::Thumbnail, url, &found);
		return Some(found);
	}
	// Only genuine network results are cached; the rule-table fallback is
	// free and must not pin a stale result into the cache.
	match super::fetch::fetch_thumbnail(url) {
		Some(found) => {
			super::cache::put(MediaTarget::Thumbnail, url, &found);
			crate::log_trace!("fetch_thumbnail resolved for {url:?} → {found:?}");
			Some(found)
		}
		None => {
			crate::log_trace!(
				"fetch_thumbnail: no og:image scraped for {url:?}, falling back to rule table"
			);
			thumbnail(url)
		}
	}
}

/// What a save resolves to and stores in the `favicon` / `thumbnail`
/// columns. Both may be `None` (no thumbnail rule, no network result).
///
/// Decoupled from persistence so the HTTP layer can resolve media *before*
/// touching the writer connection: a network fetch (cache-miss YouTube
/// avatar, a `Fetch`-mode scrape) must never hold the write mutex. The
/// `resolve_*` functions are the single home of the precedence logic, so
/// every save path resolves media identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMedia {
	pub favicon: Option<String>,
	pub thumbnail: Option<String>,
}

/// Rejects an explicit favicon/thumbnail payload that literally equals a
/// bundled-default token (`"\0default-favicon"` / `"\0default-thumbnail"`):
/// it would be stored verbatim and then render as the bundled asset,
/// silently corrupting what the user meant to store. Only enforced when the
/// payload would actually be used — an explicit `AssetMode` arm ignores the
/// payload entirely, so a `--mode default --favicon <token>` combo is left
/// alone.
fn reject_token_collision(
	kind: &str,
	mode: Option<AssetMode>,
	value: Option<&str>,
) -> Result<(), String> {
	if mode.is_none() && matches!(value, Some(DEFAULT_FAVICON) | Some(DEFAULT_THUMBNAIL)) {
		crate::log_error!("refusing {kind} value that collides with the bundled-default token");
		return Err(format!(
			"{kind} must not equal the internal bundled-default token"
		));
	}
	Ok(())
}

/// Resolves the favicon/thumbnail for a *new* bookmark from its fields and
/// URL. Precedence, highest first:
///   1. an explicit asset mode (`favicon_mode` / `thumbnail_mode`) —
///      `Auto` re-derives, `Default` stores the bundled-asset token,
///      `Fetch` has the server scrape the page at save time;
///   2. an explicit payload value (the HTTP body, an import) still wins
///      over the derivation;
///   3. the empty string is the "no custom media" sentinel: favicon falls
///      back to the *generic* domain `favicon.ico` (skipping site-specific
///      rules), thumbnail stays `None`;
///   4. otherwise auto-derived (cache-first for site-fetcher URLs).
pub fn resolve_new(new: &NewBookmark) -> Result<ResolvedMedia, String> {
	reject_token_collision("favicon", new.favicon_mode, new.favicon.as_deref())?;
	reject_token_collision("thumbnail", new.thumbnail_mode, new.thumbnail.as_deref())?;

	let favicon_source = match new.favicon_mode {
		Some(AssetMode::Auto) => "mode=auto",
		Some(AssetMode::Default) => "mode=default",
		Some(AssetMode::Fetch) => "mode=fetch",
		None if new.favicon.as_deref() == Some("") => {
			"default-favicon sentinel (--no-custom-favicon)"
		}
		None if new.favicon.is_some() => "explicit payload",
		None => "auto-derived",
	};
	let thumbnail_source = match new.thumbnail_mode {
		Some(AssetMode::Auto) => "mode=auto",
		Some(AssetMode::Default) => "mode=default",
		Some(AssetMode::Fetch) => "mode=fetch",
		None if new.thumbnail.as_deref() == Some("") => "no-thumbnail sentinel (--no-thumbnail)",
		None if new.thumbnail.is_some() => "explicit payload",
		None => "auto-derived",
	};
	crate::log_trace!(
		"resolving media for new bookmark ({url:?}): favicon {favicon_source}, thumbnail {thumbnail_source}",
		url = new.url
	);
	let favicon = match new.favicon_mode {
		Some(AssetMode::Auto) => resolve_favicon(&new.url),
		Some(AssetMode::Default) => Some(DEFAULT_FAVICON.to_string()),
		Some(AssetMode::Fetch) => fetch_favicon(&new.url),
		None => match new.favicon.as_deref() {
			Some("") => default_favicon(&new.url),
			Some(_) => new.favicon.clone(),
			None => resolve_favicon(&new.url),
		},
	};
	let thumbnail = match new.thumbnail_mode {
		Some(AssetMode::Auto) => resolve_thumbnail(&new.url),
		Some(AssetMode::Default) => Some(DEFAULT_THUMBNAIL.to_string()),
		Some(AssetMode::Fetch) => fetch_thumbnail(&new.url),
		None => match new.thumbnail.as_deref() {
			Some("") => None,
			Some(_) => new.thumbnail.clone(),
			None => resolve_thumbnail(&new.url),
		},
	};
	crate::log_debug!(
		"resolved media for new bookmark ({url:?}): favicon={favicon:?} thumbnail={thumbnail:?}",
		url = new.url
	);
	Ok(ResolvedMedia { favicon, thumbnail })
}

/// Resolves the favicon/thumbnail for a *partial update* against the
/// bookmark's current state. A URL change recomputes both from the *new*
/// URL so the stored icons can't point at the old site (a thumbnail from
/// the old URL is worse than none, so it's cleared when the new URL has no
/// thumbnail rule). Precedence, highest first:
///   1. an explicit asset mode (`favicon_mode` / `thumbnail_mode`) —
///      `Auto` re-derives from the post-update URL, `Default` resets to
///      the bundled-asset token, `Fetch` re-scrapes the page now;
///   2. an explicit value in this update request always wins;
///   3. the empty string is the "no custom media" sentinel
///      (`--no-custom-favicon` / `--no-thumbnail`): it *resets* favicon
///      to the generic domain `favicon.ico` (of the current or new URL)
///      and clears the thumbnail, regardless of URL change;
///   4. `refresh` (--refresh) re-fetches both from the (current or new)
///      URL, bypassing the fetched-media cache;
///   5. an *actual* URL change recomputes both from the new URL (a resend
///      of the identical value is not a change);
///   6. a non-URL update keeps the stored values, still accurate.
///
/// `url_for_media` is the URL the stored icons must describe: the new one
/// when this update actually changes it, otherwise the existing one.
pub fn resolve_update(
	existing: &Bookmark,
	update: &UpdateBookmark,
) -> Result<ResolvedMedia, String> {
	reject_token_collision("favicon", update.favicon_mode, update.favicon.as_deref())?;
	reject_token_collision(
		"thumbnail",
		update.thumbnail_mode,
		update.thumbnail.as_deref(),
	)?;

	let url_changed = update.url.as_deref().is_some_and(|u| u != existing.url);
	let url_for_media = match &update.url {
		Some(u) if u != &existing.url => u,
		_ => &existing.url,
	};
	let favicon_source = match update.favicon_mode {
		Some(AssetMode::Auto) => "mode=auto",
		Some(AssetMode::Default) => "mode=default",
		Some(AssetMode::Fetch) => "mode=fetch",
		None if update.favicon.as_deref() == Some("") => {
			"reset-to-default sentinel (--no-custom-favicon)"
		}
		None if update.favicon.is_some() => "explicit payload",
		None if update.refresh => "refreshed (--refresh)",
		None if url_changed => "recomputed (URL changed)",
		None => "kept stored",
	};
	let thumbnail_source = match update.thumbnail_mode {
		Some(AssetMode::Auto) => "mode=auto",
		Some(AssetMode::Default) => "mode=default",
		Some(AssetMode::Fetch) => "mode=fetch",
		None if update.thumbnail.as_deref() == Some("") => "clear sentinel (--no-thumbnail)",
		None if update.thumbnail.is_some() => "explicit payload",
		None if update.refresh => "refreshed (--refresh)",
		None if url_changed => "recomputed (URL changed)",
		None => "kept stored",
	};
	crate::log_trace!(
		"resolving media for update of #{}: favicon {favicon_source}, thumbnail {thumbnail_source}",
		existing.id
	);
	let favicon = match update.favicon_mode {
		Some(AssetMode::Auto) => resolve_favicon(url_for_media),
		Some(AssetMode::Default) => Some(DEFAULT_FAVICON.to_string()),
		Some(AssetMode::Fetch) => fetch_favicon(url_for_media),
		None => match update.favicon.as_deref() {
			Some("") => default_favicon(url_for_media),
			Some(_) => update.favicon.clone(),
			None if update.refresh => fetch_favicon_fresh(url_for_media),
			None if url_changed => resolve_favicon(url_for_media),
			None => existing.favicon.clone(),
		},
	};
	let thumbnail = match update.thumbnail_mode {
		Some(AssetMode::Auto) => resolve_thumbnail(url_for_media),
		Some(AssetMode::Default) => Some(DEFAULT_THUMBNAIL.to_string()),
		Some(AssetMode::Fetch) => fetch_thumbnail(url_for_media),
		None => match update.thumbnail.as_deref() {
			Some("") => None,
			Some(_) => update.thumbnail.clone(),
			None if update.refresh => fetch_thumbnail_fresh(url_for_media),
			None if url_changed => resolve_thumbnail(url_for_media),
			None => existing.thumbnail.clone(),
		},
	};
	crate::log_debug!(
		"resolved media for update of #{}: favicon={favicon:?} thumbnail={thumbnail:?}",
		existing.id
	);
	if favicon.as_deref() == Some(DEFAULT_FAVICON)
		&& existing.favicon.is_some()
		&& existing.favicon.as_deref() != Some(DEFAULT_FAVICON)
	{
		crate::log_warn!(
			"update #{}: custom favicon replaced with the bundled default",
			existing.id
		);
	}
	if thumbnail.as_deref() == Some(DEFAULT_THUMBNAIL)
		&& existing.thumbnail.is_some()
		&& existing.thumbnail.as_deref() != Some(DEFAULT_THUMBNAIL)
	{
		crate::log_warn!(
			"update #{}: custom thumbnail replaced with the bundled default",
			existing.id
		);
	}
	Ok(ResolvedMedia { favicon, thumbnail })
}

#[cfg(test)]
mod tests {
	use super::*;

	// Table-driven over the whole rule table: every `examples` pair must
	// pass, so a new `ROWS` entry ships with its own proof. The `checked`
	// counter guards against an accidentally emptied table (the harness
	// would otherwise trivially pass with zero rules).
	#[test]
	fn every_rule_has_passing_examples() {
		let mut checked = 0;
		for rules in SITE_RULES {
			for rule in *rules {
				for (url, expected) in rule.examples {
					let got = (rule.extract)(url);
					assert_eq!(
						got.as_deref(),
						*expected,
						"extractor for {:?} on {url:?}",
						rule.host_suffix
					);
					checked += 1;
				}
			}
		}
		// The table is never accidentally emptied.
		assert!(checked >= 3, "only {checked} rule examples checked");
	}

	#[test]
	fn youtube_watch_thumbnail() {
		let url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
		assert_eq!(
			thumbnail(url).as_deref(),
			Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg")
		);
	}

	// Query order must not matter to the `v` extractor.
	#[test]
	fn youtube_watch_with_extra_query() {
		let url = "https://youtube.com/watch?t=30&v=AbCdEf123";
		assert_eq!(
			thumbnail(url).as_deref(),
			Some("https://i.ytimg.com/vi/AbCdEf123/hqdefault.jpg")
		);
	}

	#[test]
	fn youtube_shorts_thumbnail() {
		let url = "https://www.youtube.com/shorts/AbCdEf123";
		assert_eq!(
			thumbnail(url).as_deref(),
			Some("https://i.ytimg.com/vi/AbCdEf123/hqdefault.jpg")
		);
	}

	// Channel pages have a favicon rule that returns `None`, so resolution
	// must fall through to the domain fallback — not stop at the rule.
	#[test]
	fn youtube_channel_uses_domain_fallback() {
		let url = "https://www.youtube.com/@SomeChannel";
		assert_eq!(
			favicon(url).as_deref(),
			Some("https://www.youtube.com/favicon.ico")
		);
		// Channel pages have no thumbnail rule.
		assert_eq!(thumbnail(url), None);
	}

	// A site with no rules at all still gets a favicon (domain fallback)
	// and never a thumbnail.
	#[test]
	fn generic_site_gets_domain_fallback() {
		let url = "https://example.org/some/path?q=1";
		assert_eq!(
			favicon(url).as_deref(),
			Some("https://example.org/favicon.ico")
		);
		assert_eq!(thumbnail(url), None);
	}

	// Scheme-less URLs (as pasted by a user) must still resolve; the
	// fallback picks `https` for them.
	#[test]
	fn scheme_less_url_still_matches() {
		let url = "example.org/watch?v=x";
		assert_eq!(
			favicon(url).as_deref(),
			Some("https://example.org/favicon.ico")
		);
	}

	// `default_favicon` skips the rule table entirely and always yields the
	// domain fallback — even for a URL that carries a favicon rule (the
	// `/@channel` rule here). This is the `--no-custom-favicon` contract.
	#[test]
	fn default_favicon_ignores_custom_rules() {
		let url = "https://www.youtube.com/@SomeChannel";
		assert_eq!(
			default_favicon(url).as_deref(),
			Some("https://www.youtube.com/favicon.ico")
		);
		assert_eq!(
			favicon(url).as_deref(),
			Some("https://www.youtube.com/favicon.ico")
		);
		let scheme_less = "example.org/path";
		assert_eq!(
			default_favicon(scheme_less).as_deref(),
			Some("https://example.org/favicon.ico")
		);
	}

	// The anti-false-positive guarantee: a `youtube.com` token anywhere in
	// the path of a *different* host must not trigger the YouTube rule.
	#[test]
	fn host_suffix_does_not_cross_domains() {
		// Host not matching is a no-match even though the path would.
		let url = "https://evil.example/youtube.com/watch?v=dQw4w9WgXcQ";
		assert_eq!(thumbnail(url), None);
		assert_eq!(
			favicon(url).as_deref(),
			Some("https://evil.example/favicon.ico")
		);
	}

	// Subdomains match the suffix, but a sibling domain whose name merely
	// *ends in* the suffix does not.
	#[test]
	fn subdomain_matches_but_sibling_domain_does_not() {
		assert!(thumbnail("https://www.youtube.com/watch?v=a").is_some());
		assert!(thumbnail("https://m.youtube.com/watch?v=a").is_some());
		assert_eq!(
			thumbnail("https://youtube.com.evil.example/watch?v=a"),
			None
		);
	}

	// Hosts are case-insensitive on the wire.
	#[test]
	fn host_matching_is_case_insensitive() {
		assert!(thumbnail("https://www.YOUTUBE.COM/watch?v=a").is_some());
	}

	// `fetch_favicon` degrades to the domain fallback when the network
	// fetch fails. Port 1 is almost never listening — connecting to it gets
	// a quick connection-refused, so this test needs no network or flaky
	// public service. The fallback contract: a favicon is always stored.
	#[test]
	fn fetch_favicon_falls_back_on_connection_refused() {
		let url = "http://127.0.0.1:1/";
		assert_eq!(
			fetch_favicon(url).as_deref(),
			Some("http://127.0.0.1:1/favicon.ico")
		);
		// And the scraped thumbnail stays None — no site fetcher or offline
		// rule matches 127.0.0.1, and the generic scrape fails.
		assert_eq!(fetch_thumbnail(url), None);
	}

	// A cached successful fetch short-circuits the network: prepopulate the
	// cache for a connection-refused URL and assert the stored value comes
	// back untouched. This proves `fetch_*` consult `core::cache` before
	// hitting the network.
	#[test]
	fn fetch_favicon_serves_cached_results_without_network() {
		let url = "http://127.0.0.1:1/cached-favicon";
		crate::core::cache::evict(url);
		crate::core::cache::put(MediaTarget::Favicon, url, "https://cached.example/icon.png");
		assert_eq!(
			fetch_favicon(url).as_deref(),
			Some("https://cached.example/icon.png")
		);
		// The thumbnail cache is independent (target-scoped).
		crate::core::cache::put(
			MediaTarget::Thumbnail,
			url,
			"https://cached.example/poster.jpg",
		);
		assert_eq!(
			fetch_thumbnail(url).as_deref(),
			Some("https://cached.example/poster.jpg")
		);

		// `--refresh` bypasses the cache read; the fetch fails (connection
		// refused) and degrades to the domain fallback...
		assert_eq!(
			fetch_favicon_fresh(url).as_deref(),
			Some("http://127.0.0.1:1/favicon.ico")
		);
		// ...but the offline fallback is *not* cached, so the earlier
		// successful value is still served afterwards.
		assert_eq!(
			fetch_favicon(url).as_deref(),
			Some("https://cached.example/icon.png")
		);
	}

	// --- default resolution (`resolve_*`, cache-first for YouTube) -------

	// Non-YouTube sites stay fully offline — `resolve_*` is identical to
	// the plain rule-table resolution for them.
	#[test]
	fn resolve_non_youtube_stays_offline() {
		let url = "https://example.org/some/path?q=1";
		assert_eq!(
			resolve_favicon(url).as_deref(),
			Some("https://example.org/favicon.ico")
		);
		assert_eq!(resolve_thumbnail(url), None);
	}

	// A cached successful YouTube resolution is copied straight out of the
	// cache — no network, no fetcher. This is the "always check the cache
	// first" contract for channel *and* video URLs.
	#[test]
	fn resolve_serves_cached_youtube_media_without_network() {
		let video = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
		crate::core::cache::evict(video);
		crate::core::cache::put(
			MediaTarget::Favicon,
			video,
			"https://yt3.googleusercontent.com/cached-avatar",
		);
		crate::core::cache::put(
			MediaTarget::Thumbnail,
			video,
			"https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg",
		);
		assert_eq!(
			resolve_favicon(video).as_deref(),
			Some("https://yt3.googleusercontent.com/cached-avatar")
		);
		assert_eq!(
			resolve_thumbnail(video).as_deref(),
			Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg")
		);

		let channel = "https://www.youtube.com/@CachedChannel";
		crate::core::cache::evict(channel);
		crate::core::cache::put(
			MediaTarget::Favicon,
			channel,
			"https://yt3.googleusercontent.com/cached-channel",
		);
		assert_eq!(
			resolve_favicon(channel).as_deref(),
			Some("https://yt3.googleusercontent.com/cached-channel")
		);
	}

	// Channel pages have no thumbnail; the default resolution must not go
	// to the network (or the rule table) to conclude that.
	#[test]
	fn resolve_channel_thumbnail_stays_offline() {
		let channel = "https://www.youtube.com/@SomeChannel";
		assert_eq!(resolve_thumbnail(channel), None);
	}

	// --- site fetcher target-scoped dispatch -----------------------------

	fn fav_match(url: &str) -> bool {
		url.contains("fav.site")
	}

	fn fav_fetch(_url: &str) -> Option<String> {
		Some("https://fav.site/icon.png".to_string())
	}

	fn thumb_match(url: &str) -> bool {
		url.contains("thumb.site")
	}

	fn thumb_fetch(_url: &str) -> Option<String> {
		Some("https://thumb.site/poster.jpg".to_string())
	}

	#[test]
	fn site_fetchers_are_target_scoped() {
		let fetchers: &[SiteFetcher] = &[
			SiteFetcher {
				name: "favicon-only",
				target: MediaTarget::Favicon,
				matches: fav_match,
				fetch: fav_fetch,
			},
			SiteFetcher {
				name: "thumb-only",
				target: MediaTarget::Thumbnail,
				matches: thumb_match,
				fetch: thumb_fetch,
			},
		];
		// A favicon-target fetcher resolves favicons but is ignored for
		// thumbnails, and vice versa.
		assert_eq!(
			run_site_fetchers(fetchers, "https://fav.site/x", MediaTarget::Favicon),
			Some("https://fav.site/icon.png".to_string())
		);
		assert_eq!(
			run_site_fetchers(fetchers, "https://fav.site/x", MediaTarget::Thumbnail),
			None
		);
		assert_eq!(
			run_site_fetchers(fetchers, "https://thumb.site/y", MediaTarget::Thumbnail),
			Some("https://thumb.site/poster.jpg".to_string())
		);
		assert_eq!(
			run_site_fetchers(fetchers, "https://thumb.site/y", MediaTarget::Favicon),
			None
		);
	}

	// `site_fetcher_matches` is the generality switch behind `resolve_*`:
	// it answers "would a registered fetcher handle this URL?" without
	// running any fetch. A future site is one `SiteFetcher` entry away from
	// getting cache-first default resolution.
	#[test]
	fn site_fetcher_matches_checks_url_without_fetching() {
		fn fetches_anything(_url: &str) -> Option<String> {
			panic!("site_fetcher_matches must not run the fetcher")
		}
		let fetchers: &[SiteFetcher] = &[SiteFetcher {
			name: "new-site",
			target: MediaTarget::Favicon,
			matches: fav_match,
			fetch: fetches_anything,
		}];
		assert!(site_fetcher_matches(
			fetchers,
			"https://fav.site/anything",
			MediaTarget::Favicon
		));
		assert!(!site_fetcher_matches(
			fetchers,
			"https://other.site/anything",
			MediaTarget::Favicon
		));
		assert!(!site_fetcher_matches(
			fetchers,
			"https://fav.site/anything",
			MediaTarget::Thumbnail
		));
	}

	// A matching fetcher returning `None` (fetch failed) falls through to
	// the next fetcher, and a non-matching host is skipped entirely.
	#[test]
	fn site_fetchers_fall_through_on_none() {
		fn always_match(_url: &str) -> bool {
			true
		}
		fn sometimes(_url: &str) -> Option<String> {
			None
		}
		fn fallback(_url: &str) -> Option<String> {
			Some("https://fallback.site/ok.png".to_string())
		}
		fn never_match(_url: &str) -> bool {
			false
		}
		fn should_not_run(_url: &str) -> Option<String> {
			panic!("a non-matching fetcher must not be consulted")
		}
		let fetchers: &[SiteFetcher] = &[
			SiteFetcher {
				name: "first",
				target: MediaTarget::Thumbnail,
				matches: always_match,
				fetch: sometimes,
			},
			SiteFetcher {
				name: "second",
				target: MediaTarget::Thumbnail,
				matches: always_match,
				fetch: fallback,
			},
			SiteFetcher {
				name: "never",
				target: MediaTarget::Thumbnail,
				matches: never_match,
				fetch: should_not_run,
			},
		];
		assert_eq!(
			run_site_fetchers(fetchers, "https://anything.site/x", MediaTarget::Thumbnail),
			Some("https://fallback.site/ok.png".to_string())
		);
	}

	// The real table's YouTube entry is a Favicon-target fetcher, so a
	// channel URL under the Thumbnail target is skipped by the target guard
	// without touching the network.
	#[test]
	fn youtube_fetcher_is_favicon_target_only() {
		let url = "https://www.youtube.com/@SomeChannel";
		assert_eq!(
			run_site_fetchers(SITE_FETCHERS, url, MediaTarget::Thumbnail),
			None
		);
	}
}
