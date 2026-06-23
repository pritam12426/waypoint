/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! URL helpers shared by the checker and the media layer.
//!
//! Tiny by design — this module holds the few URL predicates that more than
//! one layer needs, so they don't get re-implemented (and drift) in
//! `cmd`, `http`, or `database`. Heavier URL parsing lives in
//! `shared::extract_domain` and the media engine's `scheme_and_host`;
//! nothing here needs a full `url`-crate dependency.

/// Whether a URL is worth network-probing: only http/https links can be
/// checked for liveness. `javascript:`, `mailto:`, `file:` and friends are
/// skipped by `check`.
///
/// Prefix matching (rather than scheme parsing) is intentionally loose —
/// enough to classify the common cases and cheap to read. Note this is a
/// *liveness gate*, not a URL validator: `http://` anywhere at the start
/// passes, which is the desired behaviour for the checker's "do I even
/// bother fetching this?" question.
pub fn is_http_url(url: &str) -> bool {
	url.starts_with("http://") || url.starts_with("https://")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn http_schemes_only() {
		assert!(is_http_url("http://example.com"));
		assert!(is_http_url("https://example.com"));
		assert!(!is_http_url("ftp://example.com"));
		assert!(!is_http_url("mailto:a@b.c"));
		assert!(!is_http_url("javascript:void(0)"));
		assert!(!is_http_url(""));
	}
}
