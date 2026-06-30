/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Best-effort network discovery of a page's favicon and OpenGraph image.
//!
//! This backs the `Fetch` asset mode: at save time the server downloads the
//! bookmark's page, extracts `<link rel=icon>` / `og:image`, and stores the
//! *discovered absolute URL* — the browser renders it directly, no blob
//! storage or proxy route needed.
//!
//! Everything is deliberately best-effort. The public fetchers return
//! `Option`, not `Result`: any failure — DNS, connect, timeout, 4xx/5xx, an
//! oversized body, or an HTML page without the right tags — degrades
//! silently (with a `log_warn!`) and the caller falls back to its
//! auto-resolution. The one exception is a response body that cannot be
//! decoded at all: that is a genuine anomaly rather than an expected
//! page-level failure, so it is logged at `log_error!` (it still degrades
//! the same way — a saved bookmark must never fail because the server
//! couldn't reach or read the page).
//!
//! Hard caps, mirroring the checker: a 5s overall timeout budget, at most 5
//! redirects, http/https only (never `file:`, `data:`, `javascript:`), and
//! a 512 KB body ceiling so a huge page can't be read into memory.
//!
//! The engine is SSRF-hardened via the shared [`SsrfResolver`]: it filters
//! every resolved address and refuses to connect to loopback, private,
//! link-local, or unique-local networks on each connect and redirect hop.
//! The same resolver guards the link checker, so every URL a user can save
//! is probed under one policy (see `super::ssrf`).
//!
//! Site-specific fetchers (a YouTube channel's avatar, which only exists in
//! `ytInitialData`) live in `super::sites` and are dispatched by
//! `super::media` before the generic scrape — this module stays a generic
//! engine and never needs to know about any particular site.
//!
//! The extractors are pure functions over a `&str`, unit-tested with
//! hard-coded snippets — no network in tests.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use ureq::Agent;
use ureq::config::Config;
use ureq::unversioned::transport::DefaultConnector;

use crate::core::ssrf::SsrfResolver;

/// Overall timeout budget for one media fetch (DNS, connect, and global
/// deadline). Short on purpose: this runs inline at save time, so a slow
/// page shouldn't stall the bookmark write for long.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Redirects followed before giving up.
const MAX_REDIRECTS: u32 = 5;

/// Cap on the fetched HTML body. Anything larger is treated as a failure —
/// a page that big is not worth slurping just to find two tags.
const MAX_BODY_BYTES: u64 = 512 * 1024;

/// Total attempts per fetch (1 original + retries). Transient failures —
/// timeouts, DNS, connect resets, 429/5xx statuses — get the extra attempt;
/// definitive failures (404, unsupported scheme) fail immediately.
const RETRY_ATTEMPTS: usize = 2;

/// Pause between retry attempts. Short: a page that just dropped a packet is
/// usually reachable again in a blink, and fetch runs inline at save time.
const RETRY_BACKOFF: Duration = Duration::from_millis(150);

/// Consecutive connection-level failures to a host that trip its circuit
/// breaker. A host being down (connect refused, DNS dead, every attempt
/// timing out) is not going to heal in the 150ms between retries, so the
/// breaker stops hammering it for a while.
const BREAKER_THRESHOLD: u32 = 5;

/// How long a tripped breaker blocks fetches to that host. Long enough that
/// a genuinely-down host stops wasting save-time, short enough that a host
/// that comes back is retried soon after.
const BREAKER_COOLDOWN: Duration = Duration::from_secs(30);

/// True when an `ureq` error is worth a retry: the network is flaky or the
/// server is overloaded, and a second attempt has a real chance. HTTP 429
/// and 5xx are transient *for retry* but never trip the circuit breaker
/// (the server clearly responded); connection-level failures are both.
fn is_transient(err: &ureq::Error) -> bool {
	match err {
		ureq::Error::StatusCode(code) => *code == 429 || (500..=599).contains(code),
		ureq::Error::Timeout(_)
		| ureq::Error::Io(_)
		| ureq::Error::HostNotFound
		| ureq::Error::ConnectionFailed
		| ureq::Error::Protocol(_)
		| ureq::Error::Tls(_)
		| ureq::Error::ConnectProxyFailed(_)
		| ureq::Error::Decompress(_, _) => true,
		_ => false,
	}
}

/// True when the failure means the host itself is unreachable (as opposed
/// to a response we didn't like). Only these trip the circuit breaker —
/// a 404 or a blocked-IP refusal is the *page's* fault, not the host's.
fn is_conn_failure(err: &ureq::Error) -> bool {
	matches!(
		err,
		ureq::Error::Timeout(_)
			| ureq::Error::Io(_)
			| ureq::Error::HostNotFound
			| ureq::Error::ConnectionFailed
			| ureq::Error::Protocol(_)
			| ureq::Error::Tls(_)
			| ureq::Error::ConnectProxyFailed(_)
	)
}

/// Per-host circuit breaker. Keyed by lowercase host; tracks consecutive
/// connection-level failures and refuses new fetches for `BREAKER_COOLDOWN`
/// after the threshold is crossed. Shared process-wide so a save and a link
/// check (both of which fetch the same host) cooperate instead of each
/// hammering the dead host independently.
#[derive(Default)]
struct CircuitBreaker {
	inner: Mutex<HashMap<String, (u32, Instant)>>,
}

impl CircuitBreaker {
	/// `None` means "fetch allowed"; `Some(remaining)` means the breaker is
	/// open for this host and the caller should not even try.
	fn blocked_for(&self, host: &str) -> Option<Duration> {
		let inner = self.inner.lock().unwrap();
		inner.get(host).and_then(|(_, until)| {
			let remaining = until.saturating_duration_since(Instant::now());
			(remaining > Duration::ZERO).then_some(remaining)
		})
	}

	fn record(&self, host: &str, conn_ok: bool) {
		let mut inner = self.inner.lock().unwrap();
		let entry = inner.entry(host.to_owned()).or_insert((0, Instant::now()));
		if conn_ok {
			// Any successful connection clears the streak — the host is
			// alive again, however flaky the page itself turned out to be.
			*entry = (0, Instant::now());
		} else {
			entry.0 += 1;
			if entry.0 >= BREAKER_THRESHOLD {
				entry.1 = Instant::now() + BREAKER_COOLDOWN;
				crate::log_warn!(
					"circuit breaker: host {host:?} blocked for {}s after {} consecutive connection failures",
					BREAKER_COOLDOWN.as_secs(),
					entry.0
				);
			}
		}
	}
}

static BREAKER: LazyLock<CircuitBreaker> = LazyLock::new(CircuitBreaker::default);

/// A `<link ...>` tag, attributes included (dot-all so `>` inside a quoted
/// value doesn't end the match).
static LINK_TAG_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?is)<link\b[^>]*>").expect("static LINK_TAG_RE is valid")
});

/// A `<meta ...>` tag.
static META_TAG_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r"(?is)<meta\b[^>]*>").expect("static META_TAG_RE is valid")
});

/// One HTML attribute: `name="value"`, `name='value'`, or bare `name=value`,
/// name and value captured separately. Case-insensitive (`HREF=` happens).
static ATTR_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
	regex::Regex::new(r#"(?i)([a-z][\w-]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#)
		.expect("static ATTR_RE is valid")
});

/// Builds the shared ureq agent. One agent for all fetches — cheap and
/// thread-safe. `max_redirects` caps the redirect chain, the three timeouts
/// pin the overall budget, and `SsrfResolver` refuses private/loopback
/// destinations on every hop (http/https-only is enforced separately in
/// `fetch_html_limited`).
fn agent() -> &'static Agent {
	static AGENT: LazyLock<Agent> = LazyLock::new(|| {
		let config = Config::builder()
			.timeout_global(Some(TIMEOUT))
			.timeout_connect(Some(TIMEOUT))
			.timeout_resolve(Some(TIMEOUT))
			.max_redirects(MAX_REDIRECTS)
			.build();
		Agent::with_parts(config, DefaultConnector::default(), SsrfResolver::default())
	});
	&AGENT
}

/// Fetches `url` and returns its raw text. Every network problem — wrong
/// scheme, DNS, connect, timeout, HTTP error status, or an oversized body —
/// is an `Err` with a short reason suitable for a `log_warn!`.
fn fetch_html(url: &str) -> Result<String, String> {
	fetch_html_limited(url, MAX_BODY_BYTES, None)
}

/// The authority (host, brackets trimmed, lowercased) of a URL — the circuit
/// breaker's key. Empty for unparseable URLs; the breaker treats those as
/// their own key so a malformed URL can't dodge the guard.
fn host_of(url: &str) -> String {
	url.split_once("://")
		.map(|(_, rest)| {
			let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
			let host = if let Some(stripped) = authority.strip_prefix('[') {
				// Bracket IPv6 literal — the host is up to the closing `]`
				// (anything after it is `]:port`).
				stripped.split(']').next().unwrap_or("")
			} else {
				// Bare host (optionally `host:port`).
				authority.split(':').next().unwrap_or("")
			};
			host.to_ascii_lowercase()
		})
		.unwrap_or_default()
}

/// Outcome of one fetch attempt: succeeded, failed transiently (retry has a
/// chance), or failed definitively (never retried — a 404 is a 404).
/// `conn_failure` marks the transient kind that also trips the circuit
/// breaker (the host is unreachable) as opposed to a retryable HTTP status
/// (the host answered but is overloaded).
enum Attempt {
	Ok(String),
	Transient { reason: String, conn_failure: bool },
	Final(String),
}

/// The shared fetch path: same guarantees as `fetch_html`, but with an
/// explicit body budget and an optional `User-Agent` override. Site
/// fetchers use it when they need a bigger budget than the default 512 KB
/// or a browser UA to get the page a real visitor would.
pub(crate) fn fetch_html_limited(
	url: &str,
	limit: u64,
	user_agent: Option<&str>,
) -> Result<String, String> {
	// The scheme is case-insensitive per RFC 3986 (`HTTP://` is legal), so
	// normalize before the allow-list check.
	let scheme = match url.split_once("://") {
		Some((scheme, _)) => scheme.to_ascii_lowercase(),
		None => return Err("not an absolute URL".to_string()),
	};
	if !matches!(scheme.as_str(), "http" | "https") {
		return Err(format!("unsupported scheme \"{scheme}\""));
	}

	let host = host_of(url);
	// The breaker short-circuits before the first attempt: a host that just
	// burned through its failure budget is not going to magically heal in
	// the 150ms a retry would wait, and every blocked save is a bookmark
	// write we can unblock.
	if let Some(remaining) = BREAKER.blocked_for(&host) {
		return Err(format!(
			"host {host:?} temporarily unreachable (breaker open, {remaining:?} left)"
		));
	}

	let mut last_err = String::new();
	for attempt in 0..RETRY_ATTEMPTS {
		if attempt > 0 {
			std::thread::sleep(RETRY_BACKOFF);
		}
		match attempt_once(url, limit, user_agent) {
			Attempt::Ok(body) => {
				// Any HTTP response means the host answered — reset the
				// breaker even when the body later fails to parse (that is
				// a page problem, not a host problem).
				BREAKER.record(&host, true);
				return Ok(body);
			}
			Attempt::Final(reason) => {
				BREAKER.record(&host, true);
				return Err(reason);
			}
			Attempt::Transient {
				reason,
				conn_failure,
			} => {
				last_err = reason;
				// Only connection-level failures count against the breaker;
				// a 429/5xx means the host answered and stays un-tripped.
				BREAKER.record(&host, !conn_failure);
			}
		}
	}
	Err(last_err)
}

/// One network attempt. `Transient` covers timeouts, DNS/connect failures,
/// and 429/5xx responses (the flaky-network and overloaded-server cases a
/// retry can beat); everything else is `Final`.
fn attempt_once(url: &str, limit: u64, user_agent: Option<&str>) -> Attempt {
	let mut request = agent().get(url);
	if let Some(ua) = user_agent {
		request = request.header("User-Agent", ua);
	}
	let mut res = match request.call() {
		Ok(res) => res,
		Err(e) => {
			let reason = describe_ureq_error(&e);
			if is_transient(&e) {
				return Attempt::Transient {
					reason,
					conn_failure: is_conn_failure(&e),
				};
			}
			return Attempt::Final(reason);
		}
	};
	match res.body_mut().with_config().limit(limit).read_to_string() {
		Ok(body) => Attempt::Ok(body),
		Err(e) => {
			// A body that cannot be read back is a genuine anomaly rather
			// than an expected page-level failure — the log level says so.
			crate::log_error!("fetch_html: could not read response body of {url:?}: {e}");
			Attempt::Transient {
				reason: e.to_string(),
				conn_failure: false,
			}
		}
	}
}

/// Short human-readable reason for an `ureq` failure, for `log_warn!` lines.
fn describe_ureq_error(e: &ureq::Error) -> String {
	match e {
		ureq::Error::StatusCode(code) => format!("HTTP {code}"),
		ureq::Error::Timeout(_) => "timed out".to_string(),
		other => other.to_string(),
	}
}

/// Parses the attributes of a tag into a lowercase-name → value map. Values
/// are HTML-entity-decoded (`&amp;` → `&`) since attribute text is
/// character-escaped on the wire. Order is not preserved — attribute order
/// is not meaningful for the tags we care about.
fn attr_map(tag: &str) -> std::collections::HashMap<String, String> {
	let mut map = std::collections::HashMap::new();
	for cap in ATTR_RE.captures_iter(tag) {
		let name = cap[1].to_ascii_lowercase();
		let value = cap
			.get(2)
			.or_else(|| cap.get(3))
			.or_else(|| cap.get(4))
			.map(|m| super::import_export::html_unescape(m.as_str().trim()))
			.unwrap_or_default();
		map.insert(name, value);
	}
	map
}

/// Returns `true` when a whitespace-separated `rel` value marks a favicon:
/// `icon`, `shortcut icon`, `apple-touch-icon`, `apple-touch-icon-precomposed`.
fn is_icon_rel(rel: &str) -> bool {
	rel.split_whitespace()
		.any(|t| t == "icon" || t.contains("icon"))
}

/// Finds the page's favicon URL from a `<link rel=...icon... href=...>` tag.
///
/// Priority: the first `apple-touch-icon` link (highest resolution) wins
/// outright; otherwise the first plain icon link in document order. Returns
/// the *raw* href — resolve it against the page URL with `resolve_url`.
pub fn extract_icon_href(html: &str) -> Option<String> {
	let mut first_plain: Option<String> = None;
	for m in LINK_TAG_RE.find_iter(html) {
		let attrs = attr_map(m.as_str());
		let Some(rel) = attrs.get("rel") else {
			continue;
		};
		// `rel` values are case-insensitive (`SHORTCUT ICON` happens), so
		// normalize before any token check.
		let rel = rel.to_ascii_lowercase();
		if !is_icon_rel(&rel) {
			continue;
		}
		let Some(href) = attrs.get("href").filter(|h| !h.is_empty()) else {
			continue;
		};
		if rel.split_whitespace().any(|t| t.starts_with("apple-touch")) {
			return Some(href.clone());
		}
		if first_plain.is_none() {
			first_plain = Some(href.clone());
		}
	}
	first_plain
}

/// Finds the page's social image: the first `og:image` (or `og:image:url`)
/// `<meta>` tag, falling back to `twitter:image`. Returns the raw `content`
/// value — resolve it against the page URL with `resolve_url`.
pub fn extract_og_image(html: &str) -> Option<String> {
	let mut twitter: Option<String> = None;
	for m in META_TAG_RE.find_iter(html) {
		let attrs = attr_map(m.as_str());
		let Some(prop) = attrs
			.get("property")
			.or_else(|| attrs.get("name"))
			.map(|p| p.to_ascii_lowercase())
		else {
			continue;
		};
		let Some(content) = attrs.get("content").filter(|c| !c.is_empty()) else {
			continue;
		};
		match prop.as_str() {
			"og:image" | "og:image:url" => return Some(content.clone()),
			"twitter:image" if twitter.is_none() => twitter = Some(content.clone()),
			_ => {}
		}
	}
	twitter
}

/// Resolves a possibly-relative href from a page against that page's URL,
/// returning an absolute http(s) URL to store.
///
/// Accepts absolute URLs (http/https only), protocol-relative `//host/...`,
/// root-relative `/path`, and page-directory-relative `path`. Rejects any
/// non-http(s) scheme — `data:`, `javascript:`, `mailto:`, fragment-only
/// refs, and empty values all come back `None`. Hand-rolled (no `url`
/// crate dependency), matching how the rest of the codebase treats URLs.
pub fn resolve_url(href: &str, base: &str) -> Option<String> {
	let href = href.trim();
	if href.is_empty() || href.starts_with('#') {
		return None;
	}
	// Absolute URL — only http/https survive.
	if let Some((scheme, _)) = href.split_once("://") {
		return matches!(scheme, "http" | "https").then(|| href.to_string());
	}
	// Protocol-relative: `//host/path`, inherit the base scheme.
	if let Some(rest) = href.strip_prefix("//") {
		let scheme = base.split_once("://").map(|(s, _)| s).unwrap_or("https");
		return matches!(scheme, "http" | "https").then(|| format!("{scheme}://{rest}"));
	}
	// A colon before the first `/` (outside a leading `//`) is a
	// non-hierarchical scheme like `data:` / `javascript:` — reject it.
	let first_seg_end = href.find(['/', '?', '#']).unwrap_or(href.len());
	if href[..first_seg_end].contains(':') {
		return None;
	}
	// Relative: splice against the base's scheme, authority, and directory.
	let (scheme, rest) = base.split_once("://")?;
	if !matches!(scheme, "http" | "https") {
		return None;
	}
	let (authority, base_path) = match rest.find('/') {
		Some(idx) => (&rest[..idx], &rest[idx..]),
		None => (rest, "/"),
	};
	if href.starts_with('/') {
		Some(format!("{scheme}://{authority}{href}"))
	} else {
		let dir = if base_path.ends_with('/') {
			base_path.to_string()
		} else {
			// The last segment decides the base directory: with a
			// `.`-extension it's a file (resolve against its directory);
			// without one it's treated as a directory itself (the common
			// `https://host/root` → `/root/` convention), so relatives
			// splice *under* it rather than silently dropping it.
			let last_seg = base_path.rsplit('/').next().unwrap_or("");
			if last_seg.contains('.') {
				match base_path.rfind('/') {
					Some(idx) => base_path[..=idx].to_string(),
					None => "/".to_string(),
				}
			} else {
				format!("{base_path}/")
			}
		};
		Some(format!("{scheme}://{authority}{dir}{href}"))
	}
}

/// Fetches the page and extracts its favicon as an absolute URL. `None` on
/// any failure — the caller falls back to its generic domain favicon.
///
/// This is the generic `<link rel=icon>` scrape only. Site-specific
/// fetchers (e.g. a YouTube channel's avatar) are dispatched by
/// `super::media::fetch_favicon` before this runs — see
/// `super::sites::SITE_FETCHERS`.
pub fn fetch_favicon(url: &str) -> Option<String> {
	crate::log_trace!("fetch_favicon: fetching {url:?}");
	let html = match fetch_html(url) {
		Ok(html) => html,
		Err(e) => {
			crate::log_warn!("fetch_favicon: {e} for {url:?}");
			return None;
		}
	};
	let href = extract_icon_href(&html)?;
	let resolved = resolve_url(&href, url)?;
	crate::log_trace!("fetch_favicon: {url:?} → {resolved:?}");
	Some(resolved)
}

/// Fetches the page and extracts its `og:image` as an absolute URL. `None`
/// on any failure — most pages simply have no social image.
pub fn fetch_thumbnail(url: &str) -> Option<String> {
	crate::log_trace!("fetch_thumbnail: fetching {url:?}");
	let html = match fetch_html(url) {
		Ok(html) => html,
		Err(e) => {
			crate::log_warn!("fetch_thumbnail: {e} for {url:?}");
			return None;
		}
	};
	let content = extract_og_image(&html)?;
	let resolved = resolve_url(&content, url)?;
	crate::log_trace!("fetch_thumbnail: {url:?} → {resolved:?}");
	Some(resolved)
}

#[cfg(test)]
mod tests {
	use super::*;

	// --- pure extractors, no network -------------------------------------

	#[test]
	fn icon_plain() {
		let html = r#"<html><head><link rel="icon" href="/favicon.ico"></head></html>"#;
		assert_eq!(extract_icon_href(html).as_deref(), Some("/favicon.ico"));
	}

	// `shortcut icon` is the legacy two-token form; `href` may be quoted
	// with single quotes and attributes may use different casing.
	#[test]
	fn icon_shortcut_icon_single_quoted_uppercase() {
		let html = r#"<LINK REL='SHORTCUT ICON' HREF='/a.ico'>"#;
		assert_eq!(extract_icon_href(html).as_deref(), Some("/a.ico"));
	}

	// The apple-touch-icon link wins over an earlier plain icon link.
	#[test]
	fn icon_prefers_apple_touch() {
		let html = r#"<link rel="icon" href="/favicon.ico"><link rel="apple-touch-icon" href="/apple.png">"#;
		assert_eq!(extract_icon_href(html).as_deref(), Some("/apple.png"));
	}

	// Without an apple-touch link, document order decides.
	#[test]
	fn icon_document_order() {
		let html = r#"<link rel="shortcut icon" href="/a.ico"><link rel="icon" href="/b.ico">"#;
		assert_eq!(extract_icon_href(html).as_deref(), Some("/a.ico"));
	}

	// A `preconnect`/`stylesheet` link with no rel=icon must not match.
	#[test]
	fn icon_ignores_other_link_types() {
		let html = r#"<link rel="stylesheet" href="/x.css"><link rel="icon" href="/f.ico">"#;
		assert_eq!(extract_icon_href(html).as_deref(), Some("/f.ico"));
		let css_only = r#"<link rel="preconnect" href="https://fonts.gstatic.com">"#;
		assert_eq!(extract_icon_href(css_only), None);
	}

	// Entity decoding: `&amp;` in the URL survives as `&`.
	#[test]
	fn icon_entities_are_decoded() {
		let html = r#"<link rel="icon" href="/a&amp;b.ico">"#;
		assert_eq!(extract_icon_href(html).as_deref(), Some("/a&b.ico"));
	}

	#[test]
	fn og_image_plain() {
		let html = r#"<meta property="og:image" content="https://x.example/img.png">"#;
		assert_eq!(
			extract_og_image(html).as_deref(),
			Some("https://x.example/img.png")
		);
	}

	// `og:image` wins over an earlier `twitter:image`.
	#[test]
	fn og_image_prefers_open_graph() {
		let html = r#"<meta name="twitter:image" content="/tw.png"><meta property="og:image" content="/og.png">"#;
		assert_eq!(extract_og_image(html).as_deref(), Some("/og.png"));
	}

	// `og:image:url` is accepted alongside `og:image`.
	#[test]
	fn og_image_alt_property() {
		let html = r#"<meta property="og:image:url" content="/og.png">"#;
		assert_eq!(extract_og_image(html).as_deref(), Some("/og.png"));
	}

	#[test]
	fn og_image_twitter_fallback() {
		let html = r#"<meta name="twitter:image" content="/tw.png">"#;
		assert_eq!(extract_og_image(html).as_deref(), Some("/tw.png"));
		let none = r#"<meta name="description" content="no image here">"#;
		assert_eq!(extract_og_image(none), None);
	}

	// --- URL resolution ---------------------------------------------------

	#[test]
	fn resolve_absolute_kept() {
		assert_eq!(
			resolve_url("https://cdn.example/i.png", "https://a.example/x").as_deref(),
			Some("https://cdn.example/i.png")
		);
	}

	// Non-http(s) schemes are the injection vectors — always rejected.
	#[test]
	fn resolve_rejects_bad_schemes() {
		assert_eq!(
			resolve_url("javascript:alert(1)", "https://a.example"),
			None
		);
		assert_eq!(resolve_url("data:text/html,<x>", "https://a.example"), None);
		assert_eq!(resolve_url("mailto:x@y.z", "https://a.example"), None);
		assert_eq!(resolve_url("#fragment", "https://a.example/x"), None);
		assert_eq!(resolve_url("", "https://a.example"), None);
	}

	#[test]
	fn resolve_protocol_relative() {
		assert_eq!(
			resolve_url("//cdn.example/i.png", "https://a.example/x").as_deref(),
			Some("https://cdn.example/i.png")
		);
	}

	#[test]
	fn resolve_root_relative() {
		assert_eq!(
			resolve_url("/img.png", "https://a.example/dir/page").as_deref(),
			Some("https://a.example/img.png")
		);
	}

	// A bare filename resolves against the page's *directory*, not its file.
	#[test]
	fn resolve_page_directory_relative() {
		assert_eq!(
			resolve_url("img.png", "https://a.example/dir/page.html").as_deref(),
			Some("https://a.example/dir/img.png")
		);
	}

	// A relative path without a trailing path (bare host) keeps the root.
	#[test]
	fn resolve_bare_host_base() {
		assert_eq!(
			resolve_url("img.png", "https://a.example").as_deref(),
			Some("https://a.example/img.png")
		);
	}

	// A colon inside a *later* path segment is fine — only the first
	// segment decides whether this is a scheme.
	#[test]
