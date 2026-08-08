/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Link-liveness checker: probes every active bookmark over HTTP and
//! reports the dead ones. The actual probing happens on worker threads; all
//! database access stays on the calling thread so the single `Connection`
//! is never shared across threads.
//!
//! # How a check run works
//!
//! 1. `run` loads all active bookmarks (trashed and archived excluded).
//! 2. A bounded pool of `jobs` scoped threads pulls bookmarks from a shared
//!    job queue and performs only network probes (`url_is_alive`).
//! 3. Results flow back over a second channel; the *main* thread aggregates
//!    them, prints a report, and — only if asked — soft/hard-deletes the
//!    dead links or exports them to a file.
//!
//! Deletions on the main thread are the load-bearing design decision: the
//! probe workers never touch SQLite, so the single connection is never
//! shared across threads.
//!
//! # Liveness definition
//!
//! A bookmark is *alive* if the server answers with any 2xx/3xx status
//! (ureq follows redirects). A HEAD that is rejected with 405/501 is
//! retried with GET, since HEAD is optional per RFC 7231. Timeouts,
//! DNS failures, connection errors, and 4xx/5xx all count as dead.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::time::Duration;
use ureq::Agent;

use crate::core::url::is_http_url;
use crate::database::bookmarks;
use crate::model::Bookmark;

/// How long any single DNS lookup, connection, or whole request may take
/// before the link is counted as dead. Fixed rather than a flag — a check
/// run is a batch job, and one hung URL shouldn't stall the rest of the
/// library behind it.
const TIMEOUT: Duration = Duration::from_secs(10);

/// What a worker decided about a single bookmark.
///
/// `Skipped` covers non-http(s) URLs (mailto:, javascript:, ...) — they're
/// counted and reported but never probed.
#[derive(Debug)]
enum Outcome {
	Alive,
	Skipped,
	Dead(String),
}

/// Where to write the dead links after a check run.
pub struct ExportSpec {
	pub format: ExportFormat,
	pub path: PathBuf,
}

/// File format for the dead-link report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
	Csv,
	Markdown,
}

/// Runs a check: probes every active bookmark and reports the dead ones,
/// optionally soft/hard-deleting them and/or writing them to a file.
/// Deletions happen on this (main) thread so the single `rusqlite::Connection`
/// is never shared with the worker threads — they only do network probes.
pub fn run(
	conn: &Connection,
	delete: bool,
	hard_delete: bool,
	export: Option<ExportSpec>,
	jobs: usize,
) -> Result<()> {
	let bookmarks = bookmarks::list_all_active(conn)?;
	// A `--jobs 0` is a misuse, not a feature: clamp to at least one worker
	// rather than deadlocking on a channel nobody reads.
	let jobs = jobs.max(1);
	crate::log_debug!(
		"check: probing {} bookmarks across {jobs} worker thread{}",
		bookmarks.len(),
		if jobs == 1 { "" } else { "s" }
	);

	let agent = agent();

	// Bookmark ids flow through one channel to `jobs` worker threads; the
	// (bookmark, outcome) pairs flow back on the other. `std::mpsc::Receiver`
	// isn't `Clone`, so the job side is shared behind a `Mutex` — workers
	// only hold the lock for the blocking `recv()`, never during the
	// network probe. Scoped threads mean every worker is joined before the
	// function returns.
	let (job_tx, job_rx): (mpsc::Sender<Bookmark>, mpsc::Receiver<Bookmark>) = mpsc::channel();
	let job_rx = Mutex::new(job_rx);
	let (result_tx, result_rx) = mpsc::channel();

	std::thread::scope(|scope| {
		let agent = &agent;
		for _ in 0..jobs {
			let job_rx = &job_rx;
			let result_tx = result_tx.clone();
			scope.spawn(move || {
				loop {
					// Lock only around `recv()` (blocking), not the probe —
					// that way workers contend on the mutex only long enough
					// to grab the next bookmark.
					let bm = {
						let rx = job_rx.lock().unwrap();
						rx.recv()
					};
					// `Err` means the sender was dropped → no more jobs.
					let Ok(bm) = bm else {
						break;
					};
					let outcome = if is_http_url(&bm.url) {
						match url_is_alive(agent, &bm.url) {
							Ok(_) => Outcome::Alive,
							Err(reason) => Outcome::Dead(reason),
						}
					} else {
						Outcome::Skipped
					};
					crate::log_trace!("checked #{} {:?}: {outcome:?}", bm.id, bm.url);
					let _ = result_tx.send((bm, outcome));
				}
			});
		}
		for bm in bookmarks {
			let _ = job_tx.send(bm);
		}
		// Dropping the sender tells every worker their `recv()` will never
		// return another job, so the scope waits for all of them to wind down.
		drop(job_tx);
	});
	drop(result_tx);

	// Aggregate on the main thread. Iterating `result_rx` drains every
	// result the workers sent before the scope ended.
	let mut dead: Vec<(Bookmark, String)> = Vec::new();
	let mut alive = 0usize;
	let mut skipped = 0usize;
	for (bm, outcome) in result_rx {
		match outcome {
			Outcome::Alive => alive += 1,
			Outcome::Skipped => skipped += 1,
			Outcome::Dead(reason) => dead.push((bm, reason)),
		}
	}

	let checked = alive + dead.len();
	crate::log_info!(
		"checked {checked} bookmarks, {skipped} skipped, {} dead",
		dead.len()
	);

	// Human-readable report on stdout (one line per dead link).
	if dead.is_empty() {
		println!("no dead links ({checked} bookmarks checked, {skipped} skipped)");
	} else {
		for (b, reason) in &dead {
			println!("#{:<4} {}  <{}>  — {reason}", b.id, b.title, b.url);
		}
		println!(
			"{checked} bookmarks checked, {} dead, {skipped} skipped",
			dead.len()
		);
	}

	// Deletion phase. `delete` and `hard_delete` are mutually exclusive in
	// the CLI (`conflicts_with`), but handling both here makes the function
	// robust to direct callers. `trash` is recoverable; `purge` is not.
	let mut deleted = 0;
	if delete {
		for (b, _) in &dead {
			bookmarks::trash(conn, b.id)?;
			crate::log_info!("moved bookmark #{} to trash (dead link)", b.id);
			println!("moved bookmark #{} to trash ({})", b.id, b.url);
			deleted += 1;
		}
	}
	if hard_delete {
		for (b, _) in &dead {
			bookmarks::purge(conn, b.id)?;
			crate::log_info!("purged bookmark #{} (dead link)", b.id);
			println!("purged bookmark #{} ({})", b.id, b.url);
			deleted += 1;
		}
	}

	if let Some(spec) = export {
		export_dead(&spec.path, spec.format, &dead, checked, skipped, deleted)?;
	}

	Ok(())
}

/// Returns `Ok(())` if the URL answers with a success status (2xx/3xx —
/// redirects are followed by ureq), `Err(reason)` if it's dead.
fn url_is_alive(agent: &Agent, url: &str) -> Result<(), String> {
	match agent.head(url).call() {
		Ok(_) => Ok(()),
		// HEAD is optional per RFC 7231 — when a server refuses the
		// method (405/501) retry with GET instead of counting the link dead.
		Err(ureq::Error::StatusCode(405 | 501)) => match agent.get(url).call() {
			Ok(_) => Ok(()),
			Err(e) => Err(reason(&e)),
		},
		Err(e) => Err(reason(&e)),
	}
}

/// Turns a ureq error into the short human reason shown next to a dead
/// link. Status codes are the important case (`HTTP 404`); timeouts and
/// transport errors get words instead of a wall of internal error text.
fn reason(e: &ureq::Error) -> String {
	match e {
		ureq::Error::StatusCode(code) => format!("HTTP {code}"),
		ureq::Error::Timeout(_) => "timed out".to_string(),
		other => other.to_string(),
	}
}

/// Builds the ureq agent with a fixed overall timeout budget (DNS, connect,
/// and global). One agent is shared by all workers — ureq agents are cheap
/// and thread-safe.
fn agent() -> Agent {
	let config = Agent::config_builder()
		.timeout_global(Some(TIMEOUT))
		.timeout_connect(Some(TIMEOUT))
		.timeout_resolve(Some(TIMEOUT))
		.build();
	config.into()
}

/// Writes the dead-link report to a file, or to stdout when `path` is `-`.
fn export_dead(
	path: &Path,
	format: ExportFormat,
	dead: &[(Bookmark, String)],
	checked: usize,
	skipped: usize,
	deleted: usize,
) -> Result<()> {
	let out = match format {
		ExportFormat::Csv => export_csv(dead),
		ExportFormat::Markdown => export_md(dead, checked, skipped, deleted),
	};
	if path.as_os_str() == "-" {
		super::import_export::write_output(path, &out)?;
		crate::log_info!("exported {} dead links to stdout", dead.len());
	} else {
		std::fs::write(path, out).with_context(|| format!("failed to write {}", path.display()))?;
		crate::log_info!("exported {} dead links to {}", dead.len(), path.display());
		println!("exported {} dead links to {}", dead.len(), path.display());
	}
	Ok(())
}

/// Dead-link report as CSV (id, title, url, reason).
fn export_csv(dead: &[(Bookmark, String)]) -> String {
	let mut out = String::from("id,title,url,reason\n");
	for (b, reason) in dead {
		out.push_str(&format!(
			"{},{},{},{}\n",
			b.id,
			csv_field(&b.title),
			csv_field(&b.url),
			csv_field(reason),
		));
	}
	out
}

/// Quotes a CSV field when it contains a comma, quote, or newline, doubling
/// any embedded quotes (RFC 4180).
fn csv_field(s: &str) -> String {
	if s.chars().any(|c| matches!(c, ',' | '"' | '\n' | '\r')) {
		format!("\"{}\"", s.replace('"', "\"\""))
	} else {
		s.to_string()
	}
}

/// Dead-link report as Markdown, with the run's headline numbers in the
/// intro paragraph.
fn export_md(
	dead: &[(Bookmark, String)],
	checked: usize,
	skipped: usize,
	deleted: usize,
) -> String {
	let mut out = String::from("# Dead Links\n\n");
	out.push_str(&format!(
		"{checked} bookmarks checked, {} dead, {skipped} skipped, {deleted} deleted\n\n",
		dead.len()
	));
	for (b, reason) in dead {
		out.push_str(&format!("- [{}]({}) — {reason}\n", b.title, b.url));
	}
	out
}
