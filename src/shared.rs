/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Cross-layer helpers shared by the HTTP layer and the layers beneath it.
//! This is the dedup seam: anything two layers would otherwise re-implement
//! lives here instead.
//!
//! # What lives here
//!
//! * **Validation** — `validate_limit` / `validate_offset` / `validate_id`
//!   are the *single* place page sizes and ids get checked. The HTTP layer
//!   produces the same errors from the same rules, so a request that works
//!   over one endpoint behaves identically over another.
//! * **Size caps** — `MAX_PAGE_SIZE` is a hard client-facing ceiling (an
//!   out-of-range `limit` is an error, never silently clamped);
//!   `MAX_QUERY_ROWS` is an internal safety ceiling for unpaginated reads.
//! * **Domain extraction** — a small, dependency-free hostname extractor
//!   for the `domain` column and the domain-stats views.
//! * **Keyword charset** — the one place a keyword's allowed shape is
//!   decided. Keywords become URL path segments, so only path-safe
//!   characters are allowed.
//!
//! # Design notes
//!
//! `extract_domain` is deliberately hand-rolled instead of pulling in the
//! `url` crate: it only needs to be *good enough* for grouping bookmarks,
//! not security-sensitive. Keeping it here (rather than in `database` or
//! `core`) means the HTTP layer and the persistence layer can all agree on
//! what a "domain" is without a shared dependency between them.

/// Extracts a hostname from a URL for display/grouping (the `domain`
/// column and the domain-stats views). Deliberately hand-rolled instead of
/// pulling in the `url` crate: this only needs to be good enough for
/// grouping bookmarks, not for anything security-sensitive, and it keeps
/// the dependency list smaller.
///
/// Pipeline, each step stripping one thing:
/// 1. scheme (`https://`) — everything from the first `://` onward;
/// 2. path/query/fragment — cut at the first `/`, `?`, or `#`;
/// 3. userinfo — `rsplit('@')` keeps only what comes after the last `@`;
/// 4. port — everything before the first `:`.
///
/// Returns `None` when nothing is left (e.g. an empty string or a bare
/// `://`), so the caller can fall back to storing `NULL`.
pub fn extract_domain(input: &str) -> Option<String> {
	let without_scheme = match input.find("://") {
		Some(idx) => &input[idx + 3..],
		None => input,
	};
	let host_and_rest = without_scheme
		.split(['/', '?', '#'])
		.next()
		.unwrap_or(without_scheme);
	let host = host_and_rest.rsplit('@').next().unwrap_or(host_and_rest);
	let host = host.split(':').next().unwrap_or(host);
	if host.is_empty() {
		None
	} else {
