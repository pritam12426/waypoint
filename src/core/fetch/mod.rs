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
//! The engine is also SSRF-hardened: a custom [`Resolver`] (`SsrfResolver`)
//! filters every resolved address and refuses to connect to loopback,
//! private, link-local, or unique-local networks. Because ureq consults the
//! resolver for *every* connect — including each redirect hop — a malicious
//! page cannot redirect a fetch onto `127.0.0.1`, the metadata IP
//! `169.254.169.254`, or an internal RFC 1918 host. The check runs on the
//! very addresses the connection will use, so there is no
//! resolve-then-connect race to exploit.
//!
//! Site-specific fetchers (a YouTube channel's avatar, which only exists in
//! `ytInitialData`) live in `super::sites` and are dispatched by
//! `super::media` before the generic scrape — this module stays a generic
//! engine and never needs to know about any particular site.
//!
//! The extractors are pure functions over a `&str`, unit-tested with
//! hard-coded snippets — no network in tests.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::LazyLock;
use std::time::Duration;
use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
use ureq::{Agent, http::Uri};

/// Overall timeout budget for one media fetch (DNS, connect, and global
/// deadline). Short on purpose: this runs inline at save time, so a slow
/// page shouldn't stall the bookmark write for long.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Redirects followed before giving up.
const MAX_REDIRECTS: u32 = 5;

/// Cap on the fetched HTML body. Anything larger is treated as a failure —
/// a page that big is not worth slurping just to find two tags.
const MAX_BODY_BYTES: u64 = 512 * 1024;

/// True when `host` (already lowercased, brackets stripped) names a local
/// machine that must never be fetched. These fail fast *before* DNS; the
/// address-level check in [`is_blocked_ip`] is the real guard, this is
/// defense in depth for hosts whose names imply local scope (`localhost`,
/// mDNS `.local`, RFC 8375 `.home.arpa`, ICANN-reserved `.internal`).
fn is_blocked_host(host: &str) -> bool {
	let host = host
		.trim_start_matches('[')
		.trim_end_matches(']')
		.to_ascii_lowercase();
	host == "localhost"
		|| host.ends_with(".localhost")
		|| host.ends_with(".local")
		|| host.ends_with(".internal")
		|| host.ends_with(".home.arpa")
}

/// True when an IPv4 address is a blocked (non-public) network: unspecified,
/// loopback, RFC 1918 private, link-local (incl. the `169.254.169.254` cloud
/// metadata IP), CGNAT (RFC 6598), and the IANA special-purpose ranges
/// (RFC 6890) that no real server ever lives on.
fn is_blocked_v4(ip: &Ipv4Addr) -> bool {
	let [a, b, c, _] = ip.octets();
	a == 0 // 0.0.0.0/8 — "this network"
		|| a == 10 // 10/8 — RFC 1918
		|| a == 127 // 127/8 — loopback
		|| (a == 169 && b == 254) // 169.254/16 — link-local / metadata
		|| (a == 172 && (16..=31).contains(&b)) // 172.16/12 — RFC 1918
		|| (a == 192 && b == 168) // 192.168/16 — RFC 1918
		|| (a == 100 && (64..=127).contains(&b)) // 100.64/10 — CGNAT
		|| (a == 192 && b == 0 && c == 0) // 192.0.0/24 — IETF assignments
		|| (a == 192 && b == 0 && c == 2) // 192.0.2/24 — TEST-NET-1
		|| (a == 198 && b == 18) // 198.18/15 — benchmarking
		|| (a == 198 && b == 51 && c == 100) // 198.51.100/24 — TEST-NET-2
		|| (a == 203 && b == 0 && c == 113) // 203.0.113/24 — TEST-NET-3
		|| (a >= 240) // 240/4 + 255.255.255.255 broadcast
}

/// True when an IPv6 address is a blocked (non-public) network.
fn is_blocked_v6(ip: &Ipv6Addr) -> bool {
	let s = ip.segments();
	if ip.is_unspecified() // ::
		|| ip.is_loopback() // ::1
		|| ip.is_multicast() // ff00::/8 — never a unicast target
		|| (s[0] & 0xfe00) == 0xfc00 // fc00::/7 — unique-local
		|| (s[0] & 0xffc0) == 0xfe80 // fe80::/10 — link-local
		|| (s[0] & 0xffc0) == 0xfec0
	// fec0::/10 — site-local (deprecated RFC 3879)
	{
		return true;
	}
	// IPv4-mapped `::ffff:a.b.c.d` — the real target is the embedded v4.
	if let Some(v4) = ip.to_ipv4_mapped() {
		return is_blocked_v4(&v4);
	}
	// Deprecated IPv4-compatible `::a.b.c.d` — same treatment.
	if s[..6].iter().all(|&x| x == 0) {
		let v4 = Ipv4Addr::new(
			(s[6] >> 8) as u8,
			(s[6] & 0xff) as u8,
			(s[7] >> 8) as u8,
			(s[7] & 0xff) as u8,
		);
		return is_blocked_v4(&v4);
	}
	false
}

/// True when `ip` must never be a fetch target.
fn is_blocked_ip(ip: &IpAddr) -> bool {
	match ip {
		IpAddr::V4(v4) => is_blocked_v4(v4),
		IpAddr::V6(v6) => is_blocked_v6(v6),
	}
}

/// Resolver that refuses to hand ureq a private, loopback, or link-local
/// address. Wraps the OS [`DefaultResolver`] (same DNS, same timeout
/// behavior) and filters the resolved set down to public addresses.
///
/// ureq calls this once per `connect()`, and every redirect hop re-enters
/// `connect()` — so the guard applies to the *final* destination of a
/// redirect chain, not just the URL the user typed.
#[derive(Debug, Default)]
struct SsrfResolver(DefaultResolver);

impl Resolver for SsrfResolver {
	fn resolve(
		&self,
		uri: &Uri,
		config: &Config,
		timeout: NextTimeout,
	) -> Result<ResolvedSocketAddrs, ureq::Error> {
		// Fail fast on hostnames that name a local machine.
		if let Some(host) = uri.host()
			&& is_blocked_host(host)
		{
			return Err(ureq::Error::BadUri(format!(
				"refusing to connect to blocked host {host:?}"
			)));
		}
		let addrs = self.0.resolve(uri, config, timeout)?;
		let mut public =
			ResolvedSocketAddrs::from_fn(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
		for addr in &addrs {
			if !is_blocked_ip(&addr.ip()) {
				public.push(*addr);
			}
		}
		if public.is_empty() {
			return Err(ureq::Error::BadUri(
				"refusing to connect: destination resolves to a private or loopback address"
					.to_string(),
			));
		}
		Ok(public)
	}
}

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
	let mut request = agent().get(url);
	if let Some(ua) = user_agent {
		request = request.header("User-Agent", ua);
	}
	let mut res = request.call().map_err(|e| match e {
		ureq::Error::StatusCode(code) => format!("HTTP {code}"),
		ureq::Error::Timeout(_) => "timed out".to_string(),
		other => other.to_string(),
	})?;
	res.body_mut()
		.with_config()
		.limit(limit)
		.read_to_string()
		.map_err(|e| {
			crate::log_error!("fetch_html: could not read response body of {url:?}: {e}");
			e.to_string()
		})
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
	fn resolve_colon_in_path_is_fine() {
		assert_eq!(
			resolve_url("dir/x:y.png", "https://a.example/root").as_deref(),
			Some("https://a.example/root/dir/x:y.png")
		);
	}

	// --- SSRF guard --------------------------------------------------------

	fn parse(ip: &str) -> IpAddr {
		ip.parse().unwrap()
	}

	// Every network class the fetch engine must never touch.
	#[test]
	fn blocked_ipv4_ranges() {
		for ip in [
			"0.0.0.0",
			"10.0.0.1",
			"10.255.255.255",
			"127.0.0.1",
			"127.8.8.8",
			"169.254.169.254",
			"169.254.1.1",
			"172.16.0.1",
			"172.31.255.255",
			"192.168.0.1",
			"192.168.255.255",
			"100.64.0.1",
			"100.127.255.255",
			"192.0.0.1",
			"192.0.2.1",
			"198.18.0.1",
			"198.51.100.1",
			"203.0.113.1",
			"240.0.0.1",
			"255.255.255.255",
		] {
			assert!(is_blocked_ip(&parse(ip)), "{ip} should be blocked");
		}
	}

	#[test]
	fn blocked_ipv6_ranges() {
		for ip in [
			"::",
			"::1",
			"fe80::1",
			"fec0::1",
			"fc00::1",
			"fd12::1",
			"ff02::1",
			"::ffff:127.0.0.1",
			"::ffff:10.1.2.3",
			"::ffff:169.254.169.254",
			"::10.0.0.1",
		] {
			assert!(is_blocked_ip(&parse(ip)), "{ip} should be blocked");
		}
	}

	// Public addresses must always survive the filter.
	#[test]
	fn allowed_public_addresses() {
		for ip in [
			"8.8.8.8",
			"1.1.1.1",
			"93.184.216.34",
			"2001:4860:4860::8888",
			"2606:4700::6810:84e5",
		] {
			assert!(!is_blocked_ip(&parse(ip)), "{ip} should be allowed");
		}
	}

	#[test]
	fn blocked_hostnames() {
		for host in [
			"localhost",
			"foo.localhost",
			"printer.local",
			"db.internal",
			"router.home.arpa",
		] {
			assert!(is_blocked_host(host), "{host} should be blocked");
		}
		for host in [
			"example.com",
			"example.com.localhost.evil.com",
			"cdn.example",
		] {
			assert!(!is_blocked_host(host), "{host} should be allowed");
		}
	}

	// The bracket form used in IPv6 literals must be stripped before the
	// hostname check (it is not a hostname, but must not be rejected either).
	#[test]
	fn blocked_host_brackets() {
		assert!(!is_blocked_host("[2001:db8::1]"));
		assert!(!is_blocked_host("[::1]"));
	}
}
