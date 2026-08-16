/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Visit tracking and the visit-derived statistics.
//!
//! A "visit" is one hit on a keyword shortcut (`GET /keywords/{keyword}`),
//! recorded by `record`. This module owns that write plus every read that's
//! about *usage* rather than the bookmarks themselves: per-domain counts,
//! most/never-visited bookmarks, and the domain-by-visits ranking.
//!
//! # Why visits don't churn `updated_at`
//!
//! `record` writes only `visit_count` and `last_visited_at`, and the
//! `update_bookmark_timestamp` trigger is column-scoped (`AFTER UPDATE OF
//! title, url, ...` — neither visit column is in the list). "Last modified"
//! and "last visited" are deliberately kept as separate signals.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::model::{BookmarkVisitStats, DomainCount, DomainVisitStats, NeverVisitedBookmark};

/// Bumps a bookmark's visit counter and "last visited" stamp. Note the
/// columns it touches are *not* in `update_bookmark_timestamp`'s `OF`
/// list, so visits never churn `updated_at`.
///
/// Called fire-and-forget from the keyword redirect — the HTTP handler
/// spawns this in `spawn_blocking` and returns the 307 immediately, so a
/// slow write can never delay the redirect.
pub fn record(conn: &Connection, id: i64) -> Result<()> {
	conn.execute(
		"UPDATE bookmarks SET visit_count = visit_count + 1, last_visited_at = CURRENT_TIMESTAMP
         WHERE id = ?1",
		params![id],
	)
	.context("failed to record visit")?;
	crate::log_trace!("recorded a visit on bookmark #{id}");
	Ok(())
}

/// Bookmark count per domain, most-bookmarked first — the "where am I
/// hoarding" view. Active bookmarks only (`trashed_at IS NULL`), and
/// bookmarks with no extractable domain (`domain IS NOT NULL`) are excluded
/// from the ranking entirely. `limit`/`offset` page the ranking.
pub fn domain_counts(conn: &Connection, limit: usize, offset: usize) -> Result<Vec<DomainCount>> {
	let mut stmt = conn.prepare(
		"SELECT domain, COUNT(*) as cnt FROM bookmarks
         WHERE trashed_at IS NULL AND domain IS NOT NULL
         GROUP BY domain ORDER BY cnt DESC, domain ASC LIMIT ?1 OFFSET ?2",
	)?;
	let rows = stmt.query_map(params![limit as i64, offset as i64], |row| {
		Ok(DomainCount {
			domain: row.get(0)?,
			count: row.get(1)?,
		})
	})?;
	let domains = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("domain stats -> {} domains", domains.len());
	Ok(domains)
}

/// Domains ranked by total visit count, with per-domain bookmark counts.
///
/// Unlike `domain_counts` (which ranks by how many bookmarks a domain has),
/// this ranks by cumulative `visit_count` — the "which sites do I actually
/// use" view. Ties break alphabetically. `limit`/`offset` page the ranking.
pub fn top_visited_domains(
	conn: &Connection,
	limit: usize,
	offset: usize,
) -> Result<Vec<DomainVisitStats>> {
	let mut stmt = conn.prepare(
		"SELECT domain, SUM(visit_count) as total_visits, COUNT(*) as bookmark_count
         FROM bookmarks
         WHERE trashed_at IS NULL AND domain IS NOT NULL
         GROUP BY domain
         ORDER BY total_visits DESC, domain ASC
         LIMIT ?1 OFFSET ?2",
	)?;
	let rows = stmt.query_map(params![limit as i64, offset as i64], |row| {
		Ok(DomainVisitStats {
			domain: row.get(0)?,
			total_visits: row.get(1)?,
			bookmark_count: row.get(2)?,
		})
	})?;
	let stats = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("top-visited domains -> {} rows", stats.len());
	Ok(stats)
}

/// The `limit` most-visited bookmarks — the "most used" list of the stats
/// overview. Ties break by id (older bookmark wins) for stability.
pub fn most_visited(conn: &Connection, limit: usize) -> Result<Vec<BookmarkVisitStats>> {
	let mut stmt = conn.prepare(
		"SELECT id, title, url, domain, visit_count, last_visited_at, created_at, favicon
         FROM bookmarks
         WHERE trashed_at IS NULL
         ORDER BY visit_count DESC, id ASC
         LIMIT ?1",
	)?;
	let rows = stmt.query_map([limit as i64], |row| {
		Ok(BookmarkVisitStats {
			id: row.get(0)?,
			title: row.get(1)?,
			url: row.get(2)?,
			domain: row.get(3)?,
			visit_count: row.get(4)?,
			last_visited_at: row.get(5)?,
			created_at: row.get(6)?,
			favicon: row.get(7)?,
		})
	})?;
	let stats = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("most-visited bookmarks -> {} rows", stats.len());
	Ok(stats)
}

/// The `limit` newest bookmarks — the "recently added" list of the stats
/// overview, sorted by `created_at` descending.
pub fn recently_added(conn: &Connection, limit: usize) -> Result<Vec<BookmarkVisitStats>> {
	let mut stmt = conn.prepare(
		"SELECT id, title, url, domain, visit_count, last_visited_at, created_at, favicon
         FROM bookmarks
         WHERE trashed_at IS NULL
         ORDER BY created_at DESC
         LIMIT ?1",
	)?;
	let rows = stmt.query_map([limit as i64], |row| {
		Ok(BookmarkVisitStats {
			id: row.get(0)?,
			title: row.get(1)?,
			url: row.get(2)?,
			domain: row.get(3)?,
			visit_count: row.get(4)?,
			last_visited_at: row.get(5)?,
			created_at: row.get(6)?,
			favicon: row.get(7)?,
		})
	})?;
	let stats = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("recently-added bookmarks -> {} rows", stats.len());
	Ok(stats)
}

/// Bookmarks that have never been opened — candidates for pruning.
///
/// "Never visited" = `visit_count = 0`, which is exactly the set that
/// predates the keyword-visit feature or was never opened. Newest first so
/// the freshest candidates top the list. `limit`/`offset` page the list.
pub fn never_visited(
	conn: &Connection,
	limit: usize,
	offset: usize,
) -> Result<Vec<NeverVisitedBookmark>> {
	let mut stmt = conn.prepare(
		"SELECT id, title, url, domain, created_at, favicon
         FROM bookmarks
         WHERE trashed_at IS NULL AND visit_count = 0
         ORDER BY created_at DESC
         LIMIT ?1 OFFSET ?2",
	)?;
	let rows = stmt.query_map(params![limit as i64, offset as i64], |row| {
		Ok(NeverVisitedBookmark {
			id: row.get(0)?,
			title: row.get(1)?,
			url: row.get(2)?,
			domain: row.get(3)?,
			created_at: row.get(4)?,
			favicon: row.get(5)?,
		})
	})?;
	let stats = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("never-visited bookmarks -> {} rows", stats.len());
	Ok(stats)
}
