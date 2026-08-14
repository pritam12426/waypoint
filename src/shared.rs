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
			"the limit must be between 1 and {MAX_PAGE_SIZE}, got {l}"
		)),
	}
}

/// Validates a pagination offset. `None` → 0; explicit values must be
/// non-negative (there is no upper bound — deep pagination is allowed).
pub fn validate_offset(offset: Option<i64>) -> Result<i64, String> {
	match offset {
		None => Ok(0),
		Some(o) if o >= 0 => Ok(o),
		Some(o) => Err(format!("the offset must be 0 or greater, got {o}")),
	}
}

/// Validates a bookmark id coming from a URL path. Bookmarks are
/// `AUTOINCREMENT` and start at 1, so anything below 1 is a client error.
pub fn validate_id(id: i64) -> Result<i64, String> {
	if id < 1 {
		Err(format!(
			"the bookmark id must be a positive integer, got {id}"
		))
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
	let err = || format!("a date filter expects YYYY-MM-DD[ HH:MM[:SS]] (UTC), got \"{input}\"");
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
	{
		return Err(err());
	}
	let (Ok(h), Ok(min)) = (h.parse::<u32>(), min.parse::<u32>()) else {
		return Err(err());
	};
	let sec = match sec {
		Some(s) => s.parse::<u32>().map_err(|_| err())?,
		None => 0,
	};
	if h > 23 || min > 59 || sec > 59 {
		return Err(err());
	}
	Ok(format!("{y}-{m}-{d} {h:02}:{min:02}:{sec:02}"))
}

/// Enforces that a normalized `*_after` / `*_before` pair is a sane range:
/// the after bound must not sort after the before bound. Both values are
/// already fixed-width `YYYY-MM-DD HH:MM:SS` UTC strings from
/// `parse_datetime_bound`, so plain lexicographic comparison is
/// chronological. `label` names the pair in the error message ("created",
/// "updated", "last_visited").
pub fn validate_time_range(
	after: Option<&str>,
	before: Option<&str>,
	label: &str,
) -> Result<(), String> {
	if let (Some(a), Some(b)) = (after, before)
		&& a > b
	{
		return Err(format!(
			"inverted {label} range: the after bound ({a}) is later than the before bound ({b})"
		));
	}
	Ok(())
}

/// Option-aware wrapper over `parse_datetime_bound`: a `None` input stays
/// `None` (no bound), an `Err` from the underlying parser propagates as a
/// `String`. Handy for HTTP handlers that collect a batch of optional
/// `--*-after`/`--*-before` strings into a `BookmarkFilter`.
pub fn parse_datetime_bound_option(
	input: Option<String>,
	end_of_day: bool,
) -> Result<Option<String>, String> {
	input
		.map(|value| parse_datetime_bound(&value, end_of_day))
		.transpose()
}

/// Keywords become URL path segments at `/keywords/{keyword}`, so they are
/// restricted to the same safe charset a path segment tolerates. Returns
/// `true` when the keyword is either empty (allowed — means "no keyword")
/// or made entirely of safe characters.
///
/// The empty-string case matters: the model uses `Some("")` to mean "clear
/// this keyword", so an empty keyword must still pass validation.
pub fn is_valid_keyword(keyword: &str) -> bool {
	keyword.is_empty()
		|| keyword
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
	use super::*;

	// Exercises the four-step domain pipeline end to end (scheme, path,
	// userinfo, port) in the common case.
	#[test]
	fn domain_basic() {
		assert_eq!(
			extract_domain("https://www.example.com/path?q=1").as_deref(),
			Some("www.example.com")
		);
	}

	// Scheme-less URLs (e.g. pasted without the protocol) must still work.
	#[test]
	fn domain_no_scheme() {
		assert_eq!(
			extract_domain("example.com/a").as_deref(),
			Some("example.com")
		);
	}

	// `user:pass@host:port` must yield just `host`.
	#[test]
	fn domain_strips_userinfo_and_port() {
		assert_eq!(
			extract_domain("https://user:pass@host.example:8080/x").as_deref(),
			Some("host.example")
		);
	}

	// Degenerate inputs produce `None` rather than a panic or an empty
	// string that would pollute the domain column.
	#[test]
	fn domain_empty() {
		assert_eq!(extract_domain(""), None);
		assert_eq!(extract_domain("://"), None);
	}

	// The safe charset accepts letters/digits/dots/underscores/dashes and
	// the empty string, and rejects anything that would break a URL path.
	#[test]
	fn keyword_charset() {
		assert!(is_valid_keyword("yt"));
		assert!(is_valid_keyword("my-book_2"));
		assert!(is_valid_keyword(""));
		assert!(!is_valid_keyword("bad keyword"));
		assert!(!is_valid_keyword("bad/ke"));
	}

	// Bare dates normalize to start-of-day for `*_after` and end-of-day for
	// `*_before`; `HH:MM` gains a `:00`.
	#[test]
	fn datetime_bound_normalizes() {
		assert_eq!(
			parse_datetime_bound("2024-01-05", false).as_deref().ok(),
			Some("2024-01-05 00:00:00")
		);
		assert_eq!(
			parse_datetime_bound("2024-01-05", true).as_deref().ok(),
			Some("2024-01-05 23:59:59")
		);
		assert_eq!(
			parse_datetime_bound("2024-12-31 13:14", false)
				.as_deref()
				.ok(),
			Some("2024-12-31 13:14:00")
		);
		assert_eq!(
			parse_datetime_bound("2024-01-05 09:08:07", true)
				.as_deref()
				.ok(),
			Some("2024-01-05 09:08:07")
		);
	}

	// Real-clock impossibility and format drift are rejected.
	#[test]
	fn datetime_bound_rejects_garbage() {
		for bad in [
			"",
			"2024/01/05",
			"2024-1-05",
			"2024-01-5",
			"2024-13-01",
			"2024-00-05",
			"2024-01-00",
			"2024-01-32",
			"2024-01-05 24:00",
			"2024-01-05 12:60",
			"2024-01-05 12:00:61",
			"2024-01-05 12:00:00:00",
			"not a date",
			"2024-01-05T09:00",
		] {
			assert!(
				parse_datetime_bound(bad, false).is_err(),
				"{bad:?} should fail"
			);
		}
	}

	#[test]
	fn time_range_must_not_invert() {
		assert!(validate_time_range(None, None, "created").is_ok());
		assert!(validate_time_range(Some("2024-01-05 00:00:00"), None, "created").is_ok());
		assert!(
			validate_time_range(
				Some("2024-01-05 00:00:00"),
				Some("2024-01-05 23:59:59"),
				"created"
			)
			.is_ok()
		);
		assert!(
			validate_time_range(
				Some("2024-02-01 00:00:00"),
				Some("2024-01-01 00:00:00"),
				"updated"
			)
			.is_err()
		);
	}
}
