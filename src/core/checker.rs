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
//! 3. Results flow back over a second channel; a dedicated aggregator
//!    thread tallies them, invokes the caller's progress callback with a
//!    `CheckProgress` (so the HTTP job API can stream progress), and hands
//!    a structured `CheckReport` back to `run`.
//! 4. `run` — only if asked — soft/hard-deletes the dead links on the
//!    calling thread (the one place a `Connection` is touched).
//!
//! Deletions on the calling thread are the load-bearing design decision:
//! the probe workers never touch SQLite, so a connection is never shared
//! across threads.
//!
//! # Liveness definition
//!
//! A bookmark is *alive* if the server answers with any 2xx/3xx status
//! (ureq follows redirects). A HEAD that is rejected with 405/501 is
//! retried with GET, since HEAD is optional per RFC 7231. Timeouts,
//! DNS failures, connection errors, and 4xx/5xx all count as dead.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use ureq::Agent;
use ureq::config::Config;
use ureq::unversioned::transport::DefaultConnector;

use crate::core::ssrf::SsrfResolver;
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

/// A dead link as reported to callers: the bookmark's identifying fields
/// flattened alongside the reason it was judged dead.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeadLink {
	pub id: i64,
	pub title: String,
	pub url: String,
	pub reason: String,
}

impl DeadLink {
	fn from_bookmark(b: &Bookmark, reason: String) -> Self {
		Self {
			id: b.id,
			title: b.title.clone(),
			url: b.url.clone(),
			reason,
		}
	}
}

/// The full result of a check run. Every count is the caller-facing
/// summary; `dead` holds the dead links sorted by id.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckReport {
	/// Bookmark ids actually probed (`alive + dead.len()`).
	pub checked: usize,
	pub alive: usize,
	/// Non-http(s) URLs never probed.
	pub skipped: usize,
	/// How many dead links were trashed/purged during this run.
	pub deleted: usize,
	pub dead: Vec<DeadLink>,
}

/// Live progress reported by the aggregator thread as results arrive.
#[derive(Debug, Clone, Copy)]
pub struct CheckProgress {
	/// Results processed so far (`alive + dead`).
	pub checked: usize,
	/// Total active bookmarks the run started with.
	pub total: usize,
	/// Dead links found so far.
	pub dead: usize,
}

/// The liveness verdict for a single bookmark, as returned by `check_one`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum CheckVerdict {
	/// The URL answered with a 2xx/3xx.
	Alive,
	/// The URL is dead — `reason` is the short human explanation.
	Dead { reason: String },
	/// Not probed: non-http(s) URLs (mailto:, javascript:, ...) are counted
	/// but never contacted.
	Skipped,
}

/// Runs a check: probes every active bookmark and returns the report,
/// optionally soft/hard-deleting the dead links as it goes. Deletions
/// happen on this (calling) thread so a `rusqlite::Connection` is never
/// shared with the worker threads — they only do network probes.
///
/// `on_progress` is invoked from the aggregator thread roughly once per
/// result, with cumulative counts. It must be `Send` and is dropped when
/// the run ends.
pub fn run(
	conn: &Connection,
	delete: bool,
	hard_delete: bool,
	jobs: usize,
	on_progress: impl Fn(CheckProgress) + Send + 'static,
) -> Result<CheckReport> {
	let all = bookmarks::list_all_active(conn)?;
	// A `--jobs 0` is a misuse, not a feature: clamp to at least one worker
	// rather than deadlocking on a channel nobody reads.
	let jobs = jobs.max(1);
	let total = all.len();
	crate::log_debug!(
		"check: probing {} bookmarks across {jobs} worker thread{}",
		total,
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

	// Shared aggregate state updated by the aggregator thread; read once
	// after it joins. `Arc` so both this thread and the aggregator can
	// reach it.
	let dead: Arc<Mutex<Vec<DeadLink>>> = Arc::new(Mutex::new(Vec::new()));
	let counts: Arc<Mutex<(usize, usize)>> = Arc::new(Mutex::new((0, 0))); // (alive, skipped)
	let checked = Arc::new(AtomicUsize::new(0));

	let aggregator = {
		let dead = Arc::clone(&dead);
		let counts = Arc::clone(&counts);
		let checked = Arc::clone(&checked);
		std::thread::spawn(move || {
			for (bm, outcome) in result_rx {
				match outcome {
					Outcome::Alive => counts.lock().unwrap().0 += 1,
					Outcome::Skipped => counts.lock().unwrap().1 += 1,
					Outcome::Dead(reason) => dead
						.lock()
						.unwrap()
						.push(DeadLink::from_bookmark(&bm, reason)),
				}
				let (alive, skipped) = *counts.lock().unwrap();
				let done = checked.fetch_add(1, Ordering::Relaxed) + 1;
				on_progress(CheckProgress {
					checked: done,
					total,
					dead: done - alive - skipped,
				});
			}
		})
	};

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
		for bm in all {
			let _ = job_tx.send(bm);
		}
		// Dropping the sender tells every worker their `recv()` will never
		// return another job, so the scope waits for all of them to wind down.
		drop(job_tx);
	});
	// Dropping the remaining sender clone (held by this thread) closes the
	// result channel, which ends the aggregator's loop.
	drop(result_tx);
	aggregator.join().expect("check aggregator thread panicked");

	let (alive, skipped) = *counts.lock().unwrap();
	let mut dead = dead.lock().unwrap().clone();
	drop(counts);
	dead.sort_by_key(|dl| dl.id);
	let checked_n = alive + dead.len();
	crate::log_info!(
		"checked {checked_n} bookmarks, {skipped} skipped, {} dead",
		dead.len()
	);

	// Deletion phase. `delete` and `hard_delete` are mutually exclusive by
	// contract, but handling both here makes the function robust to direct
	// callers. `trash` is recoverable; `purge` is not.
	let mut deleted = 0;
	if delete {
		for dl in &dead {
			bookmarks::trash(conn, dl.id)?;
			crate::log_info!("moved bookmark #{} to trash (dead link)", dl.id);
			deleted += 1;
		}
	}
	if hard_delete {
		for dl in &dead {
			bookmarks::purge(conn, dl.id)?;
			crate::log_info!("purged bookmark #{} (dead link)", dl.id);
			deleted += 1;
		}
	}

	Ok(CheckReport {
		checked: checked_n,
		alive,
		skipped,
		deleted,
		dead,
	})
}

/// Checks a single bookmark by id and returns its liveness verdict. Returns
/// `Ok(None)` when the id doesn't exist or the bookmark is trashed — the
/// batch run only ever checks active bookmarks, so the single check mirrors
/// that boundary. Uses the same probe rules as `run` (`url_is_alive`).
pub fn check_one(conn: &Connection, id: i64) -> Result<Option<CheckVerdict>> {
	let Some(bm) = bookmarks::get(conn, id)? else {
		return Ok(None);
	};
	if bm.trashed_at.is_some() {
		return Ok(None);
	}
	let agent = agent();
	let verdict = if is_http_url(&bm.url) {
		match url_is_alive(&agent, &bm.url) {
			Ok(()) => CheckVerdict::Alive,
			Err(reason) => CheckVerdict::Dead { reason },
		}
	} else {
		CheckVerdict::Skipped
	};
	Ok(Some(verdict))
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
/// and thread-safe. The shared [`SsrfResolver`] is wired in so probing a
/// user-saved URL can never reach a loopback/private/link-local address —
/// same policy as the media fetch engine.
fn agent() -> Agent {
	let config = Config::builder()
		.timeout_global(Some(TIMEOUT))
		.timeout_connect(Some(TIMEOUT))
		.timeout_resolve(Some(TIMEOUT))
		.build();
	Agent::with_parts(config, DefaultConnector::default(), SsrfResolver::default())
}
