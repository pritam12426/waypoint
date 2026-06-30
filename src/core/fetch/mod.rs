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
