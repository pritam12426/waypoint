/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Aggregate statistics that span more than one table or query.
//!
//! The single-table stats live in `visits` (domain/visit rankings) and
//! `tags`/`categories` (counts); this module composes those into the
//! cross-cutting views: the overview dashboard, the hygiene gaps, and the
//! monthly activity timeline. The `/api/stats*` endpoints call straight
//! into here.

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::model::{HygieneStats, MonthlyActivity, StatsOverview};

use super::categories;
use super::tags;
use super::visits;

/// The overview dashboard: headline counts plus the top few rows of each
/// breakdown, in one struct.
///
/// Counts: `total` / `starred` / `archived` count active bookmarks
/// (`trashed_at IS NULL`), `trashed` is the recycle-bin total. The lists
/// are capped at 5: categories (full), top domains, top tags, most-visited
/// bookmarks, and most-recently-added bookmarks.
pub fn overview(conn: &Connection) -> Result<StatsOverview> {
	let total: i64 = conn.query_row(
		"SELECT COUNT(*) FROM bookmarks WHERE trashed_at IS NULL",
		[],
		|row| row.get(0),
	)?;
	let starred: i64 = conn.query_row(
		"SELECT COUNT(*) FROM bookmarks WHERE trashed_at IS NULL AND starred = 1",
		[],
		|row| row.get(0),
	)?;
	let archived: i64 = conn.query_row(
		"SELECT COUNT(*) FROM bookmarks WHERE trashed_at IS NULL AND is_archived = 1",
		[],
		|row| row.get(0),
	)?;
	let trashed: i64 = conn.query_row(
		"SELECT COUNT(*) FROM bookmarks WHERE trashed_at IS NOT NULL",
		[],
		|row| row.get(0),
	)?;
	// The four sub-lists reuse the domain/tag/visit readers and just take
	// the top 5 — keeping the ordering logic in exactly one place each.
	let categories = categories::counts(conn)?;
	let top_domains = visits::domain_counts(conn, 5, 0)?;
	let top_tags = tags::list_with_counts(conn, Some(5), 0)?;
	let most_visited = visits::most_visited(conn, 5)?;
	let recently_added = visits::recently_added(conn, 5)?;

	Ok(StatsOverview {
		total,
		starred,
		archived,
		trashed,
		categories,
		top_domains,
		top_tags,
		most_visited,
		recently_added,
	})
}

/// Gaps in bookmark hygiene: how many active bookmarks are missing tags /
/// a note / a description.
///
/// "Missing" is defined per field: no tag links at all (`id NOT IN ...`),
/// or a note/description that is NULL or the empty string. The counts are
/// independent — one bookmark can count toward several gaps — and trashed
/// bookmarks are excluded entirely.
pub fn hygiene(conn: &Connection) -> Result<HygieneStats> {
	let total: i64 = conn.query_row(
		"SELECT COUNT(*) FROM bookmarks WHERE trashed_at IS NULL",
		[],
		|row| row.get(0),
	)?;
	// `NOT IN (SELECT ...)` on the junction: a bookmark with no rows in
	// bookmark_tags has no tags at all.
	let missing_tags: i64 = conn.query_row(
		"SELECT COUNT(*) FROM bookmarks WHERE trashed_at IS NULL
         AND id NOT IN (SELECT DISTINCT bookmark_id FROM bookmark_tags)",
		[],
		|row| row.get(0),
	)?;
	// Both the NULL case (never set) and the empty string (explicitly
	// cleared) count as missing.
	let missing_note: i64 = conn.query_row(
		"SELECT COUNT(*) FROM bookmarks WHERE trashed_at IS NULL
         AND (note IS NULL OR note = '')",
		[],
		|row| row.get(0),
	)?;
	let missing_description: i64 = conn.query_row(
		"SELECT COUNT(*) FROM bookmarks WHERE trashed_at IS NULL
         AND (description IS NULL OR description = '')",
		[],
		|row| row.get(0),
	)?;
	Ok(HygieneStats {
		total,
		missing_tags,
		missing_note,
		missing_description,
	})
}

/// Bookmarks added per calendar month, newest first (defaults to the most
/// recent 12 months).
///
/// Uses SQLite's `strftime('%Y-%m', ...)` so the grouping keys are already
/// `"YYYY-MM"` strings — no timezone handling in Rust. Months with zero
/// bookmarks produce no row (gaps in the timeline are simply absent).
/// `limit`/`offset` page the timeline.
pub fn monthly_activity(
	conn: &Connection,
	limit: usize,
	offset: usize,
) -> Result<Vec<MonthlyActivity>> {
	let mut stmt = conn.prepare(
		"SELECT strftime('%Y-%m', created_at) as month, COUNT(*) as cnt
         FROM bookmarks
         WHERE trashed_at IS NULL
         GROUP BY month
         ORDER BY month DESC
         LIMIT ?1 OFFSET ?2",
	)?;
	let rows = stmt.query_map(params![limit as i64, offset as i64], |row| {
		Ok(MonthlyActivity {
			month: row.get(0)?,
			count: row.get(1)?,
		})
	})?;
	let months = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("monthly activity -> {} months", months.len());
	Ok(months)
}
