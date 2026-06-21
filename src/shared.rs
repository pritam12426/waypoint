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
		Some(host.to_lowercase())
	}
}

/// Upper bound for `limit` on every list/search HTTP endpoint. Anything
/// larger is a client error (`invalid_limit`), not something to clamp.
pub const MAX_PAGE_SIZE: i64 = 1000;

/// Upper bound the SQL layer clamps unpaginated reads to (export, keyword
/// listing) — a safety ceiling, not a user-facing page size.
pub const MAX_QUERY_ROWS: i64 = 100_000;

/// Validates a page size. Returns the accepted value, or a message
/// describing what is wrong.
///
/// `None` means "client didn't send a limit" → the default of 200 is used.
/// An explicit value must be in `1..=MAX_PAGE_SIZE`; anything else is
/// rejected. The message is the user-facing part of a `400 invalid_limit`
/// response, so it reads like an instruction, not a stack trace.
pub fn validate_limit(limit: Option<i64>) -> Result<i64, String> {
	validate_limit_with_default(limit, 200)
}

/// The list-endpoint `limit` check with a caller-chosen default. The list
/// endpoints share one default (200), the stats sub-resources each carry
/// their own (50/20/12), and a search is capped at 50 — the range and error
/// contract are identical in every case.
pub fn validate_limit_with_default(limit: Option<i64>, default: i64) -> Result<i64, String> {
	match limit {
		None => Ok(default),
		Some(l) if (1..=MAX_PAGE_SIZE).contains(&l) => Ok(l),
		Some(l) => Err(format!(
			"limit must be between 1 and {MAX_PAGE_SIZE}, got {l}"
		)),
	}
}

/// Validates a pagination offset. `None` → 0; explicit values must be
/// non-negative (there is no upper bound — deep pagination is allowed).
pub fn validate_offset(offset: Option<i64>) -> Result<i64, String> {
	match offset {
		None => Ok(0),
		Some(o) if o >= 0 => Ok(o),
		Some(o) => Err(format!("offset must be 0 or greater, got {o}")),
	}
}

/// Validates a bookmark id coming from a URL path. Bookmarks are
/// `AUTOINCREMENT` and start at 1, so anything below 1 is a client error.
pub fn validate_id(id: i64) -> Result<i64, String> {
	if id < 1 {
		Err(format!("id must be a positive integer, got {id}"))
	} else {
		Ok(id)
	}
}

/// Parses a time-range bound (`YYYY-MM-DD[ HH:MM[:SS]]`, UTC) into the
/// fixed-width `YYYY-MM-DD HH:MM:SS` form the `created_*` / `updated_*` /
/// `last_visited_*` SQL filters compare lexicographically.
///
/// A bare date is normalized to the start of that day (`... 00:00:00`)
/// when `end_of_day` is false (the `*_after` bounds) and to the end of
/// that day (`... 23:59:59`) when true (the `*_before` bounds) — both ends
/// inclusive. This normalization matters: without it a bare `2024-01-05`
/// would sort *before* `2024-01-05 09:00` and silently exclude the whole
/// day it names.
///
/// The caller decides which bound it is parsing; this function only shapes
/// one bound. Pairwise `after <= before` sanity lives in
/// `validate_time_range`.
pub fn parse_datetime_bound(input: &str, end_of_day: bool) -> Result<String, String> {
	let err = || format!("expected YYYY-MM-DD[ HH:MM[:SS]] (UTC), got \"{input}\"");
	let trimmed = input.trim();
	let (date, time) = match trimmed.find(' ') {
		Some(idx) => (&trimmed[..idx], &trimmed[idx + 1..]),
		None => (trimmed, ""),
	};

	let mut date_parts = date.split('-');
	let (Some(y), Some(m), Some(d), None) = (
		date_parts.next(),
		date_parts.next(),
		date_parts.next(),
		date_parts.next(),
	) else {
		return Err(err());
	};
	if y.len() != 4
		|| m.len() != 2
		|| d.len() != 2
		|| !y.bytes().all(|b| b.is_ascii_digit())
		|| !m.bytes().all(|b| b.is_ascii_digit())
		|| !d.bytes().all(|b| b.is_ascii_digit())
	{
		return Err(err());
	}
	let (Ok(month), Ok(day)) = (m.parse::<u32>(), d.parse::<u32>()) else {
		return Err(err());
	};
	if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
		return Err(err());
	}

	if time.is_empty() {
		return Ok(if end_of_day {
			format!("{y}-{m}-{d} 23:59:59")
		} else {
			format!("{y}-{m}-{d} 00:00:00")
		});
	}

	let mut tparts = time.split(':');
	let (Some(h), Some(min), sec, None) =
		(tparts.next(), tparts.next(), tparts.next(), tparts.next())
	else {
		return Err(err());
	};
	if h.len() != 2
		|| min.len() != 2
		|| sec.is_some_and(|s| s.len() != 2)
		|| !h.bytes().all(|b| b.is_ascii_digit())
		|| !min.bytes().all(|b| b.is_ascii_digit())
		|| sec.is_some_and(|s| !s.bytes().all(|b| b.is_ascii_digit()))
