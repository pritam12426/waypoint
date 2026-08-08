/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Dev-only benchmark harness: measures how the read paths behave at scale.
//!
//! Generates `--rows` synthetic bookmarks into a (tempdir or `--keep`)
//! database through the real `database::bookmarks::insert` path — FTS
//! triggers, media resolution, duplicate checks all fire exactly as in
//! production — then times the queries that drive the web UI:
//!
//!   * list page 1 (the main screen)
//!   * list at a deep offset (worst-case pagination)
//!   * COUNT(*) (the `x-total-count` header)
//!   * full-text search (broad match)
//!   * stats overview (the dashboard)
//!   * point read by id (keyword redirect / detail view)
//!
//! Run before and after each perf phase and compare the median columns:
//!
//! ```sh
//! cargo run --example bench -- --rows 1000000 --iters 3
//! ```
//!
//! `--keep <path>` writes the database to a real file (for later load
//! tests); the path must not already exist. Also prints `EXPLAIN QUERY
//! PLAN` for the list query so index usage is visible in the same output.

use std::path::PathBuf;
use std::sync::Once;
use std::time::Instant;

use rusqlite::Connection;
use waypoint::database;
use waypoint::database::bookmarks as bm;
use waypoint::database::stats as st;
use waypoint::model::{BookmarkFilter, NewBookmark};

const ADJECTIVES: &[&str] = &[
	"astral", "cosmic", "daring", "elegant", "feral", "golden", "humble", "luminous", "nimble",
	"placid", "radiant", "silent",
];
const NOUNS: &[&str] = &[
	"falcon", "harbor", "island", "jungle", "kettle", "lantern", "meadow", "orbit", "prairie",
	"quarry", "river", "summit",
];
const TAG_POOL: &[&str] = &["rust", "web", "tools", "reading", "reference", "tutorial"];

static LOG_INIT: Once = Once::new();
fn silence_logs() {
	LOG_INIT.call_once(|| {
		waypoint::logging::log_init(
			None,
			waypoint::logging::LogLevel::Off,
			waypoint::logging::LogFormat::Pretty,
		);
	});
}

struct Args {
	rows: i64,
	iters: usize,
	keep: Option<PathBuf>,
}

fn parse_args() -> Args {
	let mut rows: i64 = 1_000_000;
	let mut iters: usize = 3;
	let mut keep: Option<PathBuf> = None;
	let mut args = std::env::args().skip(1);
	while let Some(arg) = args.next() {
		match arg.as_str() {
			"--rows" => rows = args.next().and_then(|v| v.parse().ok()).unwrap_or(rows),
			"--iters" => iters = args.next().and_then(|v| v.parse().ok()).unwrap_or(iters),
			"--keep" => keep = Some(args.next().map(PathBuf::from).expect("--keep needs a path")),
			"--help" | "-h" => {
				println!(
					"usage: bench [--rows N] [--iters N] [--keep PATH]\n  N rows (default 1000000), median of N iters (default 3)."
				);
				std::process::exit(0);
			}
			other => {
				eprintln!("unknown argument: {other}");
				std::process::exit(2);
			}
		}
	}
	Args { rows, iters, keep }
}

fn main() {
	let args = parse_args();
	silence_logs();
	println!(
		"bench: rows={} iters={} keep={}",
		args.rows,
		args.iters,
		args.keep
			.as_deref()
			.map(|p| p.display().to_string())
			.unwrap_or("<tempdir>".into())
	);

	// Resolve the database path. `--keep` demands a fresh path so every run
	// starts from a clean schema (open() migrates, never resets).
	let (_dir, path) = match &args.keep {
		Some(path) => {
			if path.exists() {
				eprintln!("--keep path already exists: {}", path.display());
				std::process::exit(2);
			}
			(None, path.clone())
		}
		None => {
			let dir = tempfile::tempdir().expect("tempdir");
			let path = dir.path().join("bench.sqlite");
			(Some(dir), path)
		}
	};

	let conn = database::open(&path).expect("open database");

	// ---- Bulk load (real insert path) ----
	let load_start = Instant::now();
	load(&conn, args.rows);
	let load_elapsed = load_start.elapsed();
	let per_sec = args.rows as f64 / load_elapsed.as_secs_f64();
	println!(
		"load {:>10} rows: {:>12?}  ({:.0} rows/sec)",
		args.rows, load_elapsed, per_sec
	);

	// Spread `created_at` across a year so ORDER BY created_at DESC is a
	// realistic (non-degenerate) sort instead of a 1-second-tie pile-up.
	// created_at is not in any FTS trigger's OF list, so this is trigger-free.
	let spread_start = Instant::now();
	conn.execute_batch(
		"UPDATE bookmarks SET created_at = datetime('now', '-' || CAST((id % 365) AS TEXT) || ' days', '-' || CAST((id % 24) AS TEXT) || ' hours');",
	)
	.expect("spread created_at");
	println!("spread created_at:    {:>12?}", spread_start.elapsed());

	// ---- Query plan for the hot list query ----
	println!("EXPLAIN QUERY PLAN (list page 1):");
	explain_list(&conn);

	// ---- Timed operations ----
	let deep_offset = (args.rows / 2).max(0);
	println!("benchmarking {} ops x {} iters...", 8, args.iters);

	bench("list page 1 (limit 20)", args.iters, || {
		let f = BookmarkFilter {
			limit: Some(20),
			..Default::default()
		};
		bm::list(&conn, &f).expect("list page 1");
	});

	bench("list deep offset (1/2)", args.iters, || {
		let f = BookmarkFilter {
			limit: Some(20),
			offset: Some(deep_offset),
			..Default::default()
		};
		bm::list(&conn, &f).expect("list deep");
	});

	// A cursor (keyset) page at the same depth: find the middle page once by
	// offset to learn its last row's (created_at, id), then time repeated
	// cursor pages against that bound. Should be ~constant time regardless
	// of depth (index range SEARCH) where the OFFSET walk scales with depth.
	let mid_cursor = {
		let f = BookmarkFilter {
			limit: Some(20),
			offset: Some(deep_offset),
			..Default::default()
		};
		let page = bm::list(&conn, &f).expect("find middle page");
		let last = page.last().expect("middle page is not empty");
		(last.created_at.clone(), last.id)
	};
	bench("list cursor page (1/2)", args.iters, || {
		let f = BookmarkFilter {
			limit: Some(20),
			before_cursor: Some(mid_cursor.clone()),
			..Default::default()
		};
		bm::list(&conn, &f).expect("list cursor page");
	});

	bench("count (x-total-count)", args.iters, || {
		bm::count(&conn, &BookmarkFilter::default()).expect("count");
	});

	bench("search 'synthetic'", args.iters, || {
		bm::search(&conn, "synthetic", 20, &BookmarkFilter::default()).expect("search");
	});

	bench("stats overview", args.iters, || {
		st::overview(&conn).expect("stats overview");
	});

	// Phase 4: the same aggregate served through the HTTP `StatsCache`. The
	// first iteration is a cold miss (≈ the raw query above); every later
	// iteration is a hash lookup with no DB work, so the median is the
	// dashboard path a repeat visitor actually pays.
	let stats_cache = waypoint::http::StatsCache::new();
	bench("stats overview (cached)", args.iters, || {
		let key = "overview".to_string();
		if stats_cache.get(&key).is_none() {
			let body =
				serde_json::to_vec(&st::overview(&conn).expect("stats overview")).expect("serde");
			stats_cache.put(&key, body);
		}
	});

	bench("get by id (middle)", args.iters, || {
		bm::get(&conn, args.rows / 2).expect("get by id");
	});

	let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
	println!("db file size: {:.1} MiB", size as f64 / (1024.0 * 1024.0));
}

/// Times `f` `iters` times and prints median + min. Queries must be
/// idempotent (they are — all benches are reads or the one-shot load).
fn bench<F: FnMut()>(name: &str, iters: usize, mut f: F) {
	let mut samples = Vec::with_capacity(iters);
	for _ in 0..iters {
		let start = Instant::now();
		f();
		samples.push(start.elapsed());
	}
	samples.sort();
	let min = samples[0];
	let med = samples[samples.len() / 2];
	println!("{name:<26} median {med:>12?}  min {min:>12?}");
}

/// Inserts `rows` synthetic bookmarks in one transaction (commit every
/// 50_000 rows so no single journal/WAL frame grows unboundedly). Uses the
/// real `insert` path: FTS triggers, duplicate pre-checks, media
/// auto-resolution (offline rule table only — explicit empty favicon/
/// thumbnail payloads keep it off the network).
fn load(conn: &Connection, rows: i64) {
	let mut tx = conn.unchecked_transaction().expect("begin load tx");
	for i in 0..rows {
		bm::insert(&tx, &bookmark_for(i)).expect("insert");
		if i % 50_000 == 49_999 {
			tx.commit().expect("commit chunk");
			tx = conn.unchecked_transaction().expect("begin next tx");
		}
	}
	tx.commit().expect("commit final");
}

/// One synthetic bookmark. Every 20th gets two tags so the `attach_tags`
/// N+1 in `list`/`search` has real rows to load; every 5th is starred.
fn bookmark_for(i: i64) -> NewBookmark {
	let adj = ADJECTIVES[(i as usize) % ADJECTIVES.len()];
	let noun = NOUNS[(i as usize) % NOUNS.len()];
	let url = format!("https://example.com/{i}/{adj}-{noun}");
	let title = format!("Synthetic Bookmark {i}: {adj} {noun}");
	let tags = if i % 20 == 0 {
		Some(vec![
			"benchmark".to_string(),
			TAG_POOL[((i / 20) as usize) % TAG_POOL.len()].to_string(),
		])
	} else {
		None
	};
	NewBookmark {
		url,
		title: Some(title),
		description: Some(format!(
			"A synthetic benchmark bookmark #{i} with a description to index for search."
		)),
		category: Some("Benchmark".to_string()),
		tags,
		keyword: None,
		note: None,
		// Empty payloads keep media resolution offline (generic favicon,
		// no thumbnail) — no network during a bench run.
		favicon: Some(String::new()),
		thumbnail: Some(String::new()),
		favicon_mode: None,
		thumbnail_mode: None,
		starred: Some(i % 5 == 0),
	}
}

/// Prints the SQLite query plan for the exact SELECT the list endpoint
/// runs — lets the before/after index usage be compared in the same output
/// as the timings.
fn explain_list(conn: &Connection) {
	let sql = "SELECT b.id FROM bookmarks b
	           LEFT JOIN categories c ON c.id = b.category_id
	           WHERE b.trashed_at IS NULL
	           ORDER BY b.created_at DESC LIMIT 20";
	let mut stmt = conn
		.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
		.expect("prepare explain");
	let mut rows = stmt.query([]).expect("run explain");
	while let Some(row) = rows.next().expect("next explain row") {
		let detail: String = row.get(3).expect("detail column");
		println!("  [plan] {detail}");
	}
}
