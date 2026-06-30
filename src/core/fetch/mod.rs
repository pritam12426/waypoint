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
