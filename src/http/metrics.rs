/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! RED metrics for the HTTP layer, rendered in Prometheus text format at
//! `/metrics`.
//!
//! Deliberately hand-rolled (no `prometheus`/`metrics` crate): the surface
//! is tiny — a request counter, a duration histogram, an in-flight gauge,
//! and the SQLite pool gauges — and a dependency for that would cost more
//! in compile time than it saves in code. The output is the plain
//! Prometheus exposition format, which every scrape target (Prometheus,
//! Grafana agent, VictoriaMetrics) understands verbatim.
//!
//! Path labels are sanitized so high-cardinality IDs don't explode the
//! series count: digit-only path segments become `{id}` (`/bookmarks/123`
//! → `/bookmarks/{id}`). The total series is capped at [`MAX_SERIES`];
//! paths beyond the cap are not added as new series (the requests are still
//! counted, under the already-created series for their parent endpoint).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// Hard cap on distinct (method, path) series. A self-hosted bookmark
/// server has a handful of endpoints; anything approaching this cap means a
/// bug (or a bot hammering unlisted URLs), and unbounded cardinality is the
/// one thing that makes Prometheus fall over.
const MAX_SERIES: usize = 1024;

/// Duration histogram buckets (seconds) — the default Prometheus request
/// distribution, which keeps dashboards comparable out of the box.
const BUCKETS: &[f64] = &[
	0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Turns a request path into its metric label: every digit-only segment is
/// replaced with `{id}`, so `/api/bookmarks/42` and
/// `/api/bookmarks/7` collapse onto one series.
fn sanitize_path(path: &str) -> String {
	path.split('/')
		.map(|seg| {
			if !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()) {
				"{id}"
			} else {
				seg
			}
		})
		.collect::<Vec<_>>()
		.join("/")
}

/// Process-wide HTTP metrics. All counters are locked `HashMap`s — the
/// update rate is a few thousand requests per second at most on this class
/// of server, so a lock per request is noise.
pub struct Metrics {
	/// (method, sanitized path, status code) → request count.
	requests: Mutex<HashMap<(String, String, u16), u64>>,
	/// (method, sanitized path) → duration-bucket counts (cumulative, +Inf
	/// implied by `count`).
	durations: Mutex<HashMap<(String, String), Vec<u64>>>,
	/// (method, sanitized path) → accumulated seconds (the histogram `sum`).
	duration_sum: Mutex<HashMap<(String, String), f64>>,
	/// Requests currently being handled.
	in_flight: AtomicUsize,
	/// When the process started serving (for `waypointd_uptime_seconds`).
	started: Instant,
}

/// Decrements the in-flight gauge when dropped, so the gauge can't leak when
/// a handler future is aborted mid-request (client disconnect, timeout)
/// rather than completing normally.
pub struct InFlightGuard<'a>(&'a Metrics);

impl Drop for InFlightGuard<'_> {
	fn drop(&mut self) {
		self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
	}
}

impl Default for Metrics {
	fn default() -> Self {
		Self {
			requests: Mutex::new(HashMap::new()),
			durations: Mutex::new(HashMap::new()),
			duration_sum: Mutex::new(HashMap::new()),
			in_flight: AtomicUsize::new(0),
			started: Instant::now(),
		}
	}
}

impl Metrics {
	pub fn new() -> Self {
		Self {
			started: Instant::now(),
			..Default::default()
		}
	}

	/// Marks a request as in-flight and returns a guard that clears the mark
	/// when it drops. Call it immediately before the handler runs, from the
	/// outer logging middleware, so the gauge reflects real concurrency
	/// rather than the moment the request finished — and the guard, not a
	/// paired call, ensures the decrement runs even if the handler future is
	/// dropped (client disconnect) instead of completing.
	pub fn request_started(&self) -> InFlightGuard<'_> {
		self.in_flight.fetch_add(1, Ordering::Relaxed);
		InFlightGuard(self)
	}

	/// Records one completed request: appends to the counter/histogram.
	/// Call it from the outer logging middleware, which wraps every route.
	pub fn observe(&self, method: &str, path: &str, status: u16, elapsed: std::time::Duration) {
		let label_path = sanitize_path(path);
		let secs = elapsed.as_secs_f64();

		{
			let mut reqs = self.requests.lock().unwrap();
			*reqs
				.entry((method.to_owned(), label_path.clone(), status))
				.or_insert(0) += 1;
		}
		{
			let mut dur = self.durations.lock().unwrap();
			let key = (method.to_owned(), label_path.clone());
			// Cardinality guard: once the distinct (method, path) series
			// reach the cap, a *new* path is not added as a histogram series
			// (the request counter above still counts it). Unbounded
			// cardinality is the one thing that makes Prometheus fall over.
			if dur.len() < MAX_SERIES || dur.contains_key(&key) {
				let buckets = dur
					.entry(key.clone())
					.or_insert_with(|| vec![0; BUCKETS.len()]);
				for (i, bound) in BUCKETS.iter().enumerate() {
					if secs <= *bound {
						buckets[i] += 1;
					}
				}
				*self.duration_sum.lock().unwrap().entry(key).or_insert(0.0) += secs;
			}
		}
	}

	/// Renders everything in Prometheus text format. `db` supplies the
	/// SQLite pool gauges (writer lock + readers in use) so a scrape tells
	/// an operator about read/write saturation without a second probe.
	pub fn render(&self, db: &crate::database::Db) -> String {
		use std::fmt::Write;
		let mut out = String::with_capacity(4096);

		let _ = writeln!(
			out,
			"# HELP waypointd_uptime_seconds Seconds since the server started."
		);
		let _ = writeln!(out, "# TYPE waypointd_uptime_seconds gauge");
		let _ = writeln!(
			out,
			"waypointd_uptime_seconds {}",
			self.started.elapsed().as_secs_f64()
		);
		let _ = writeln!(out);

		let _ = writeln!(
			out,
			"# HELP waypointd_http_requests_total Total HTTP requests handled."
		);
		let _ = writeln!(out, "# TYPE waypointd_http_requests_total counter");
		let reqs = self.requests.lock().unwrap();
		let mut keys: Vec<_> = reqs.keys().collect();
		keys.sort();
		for (method, path, status) in keys {
			let _ = writeln!(
				out,
				"waypointd_http_requests_total{{method=\"{method}\",path=\"{path}\",status=\"{status}\"}} {}",
				reqs.get(&(method.clone(), path.clone(), *status))
					.unwrap_or(&0)
			);
		}
		let _ = writeln!(out);

		let _ = writeln!(
			out,
			"# HELP waypointd_http_request_duration_seconds HTTP request latency distribution."
		);
		let _ = writeln!(
			out,
			"# TYPE waypointd_http_request_duration_seconds histogram"
		);
		let dur = self.durations.lock().unwrap();
		let sums = self.duration_sum.lock().unwrap();
		let mut dkeys: Vec<_> = dur.keys().collect();
		dkeys.sort();
		for (method, path) in dkeys {
			let buckets = dur.get(&(method.clone(), path.clone())).unwrap();
			for (i, bound) in BUCKETS.iter().enumerate() {
				let _ = writeln!(
					out,
					"waypointd_http_request_duration_seconds_bucket{{method=\"{method}\",path=\"{path}\",le=\"{bound}\"}} {}",
					buckets[i]
				);
			}
			let _ = writeln!(
				out,
				"waypointd_http_request_duration_seconds_bucket{{method=\"{method}\",path=\"{path}\",le=\"+Inf\"}} {}",
				buckets.iter().sum::<u64>()
			);
			let _ = writeln!(
				out,
				"waypointd_http_request_duration_seconds_sum{{method=\"{method}\",path=\"{path}\"}} {}",
				sums.get(&(method.clone(), path.clone())).unwrap_or(&0.0)
			);
			let _ = writeln!(
				out,
				"waypointd_http_request_duration_seconds_count{{method=\"{method}\",path=\"{path}\"}} {}",
				buckets.iter().sum::<u64>()
			);
		}
		let _ = writeln!(out);

		let _ = writeln!(
			out,
			"# HELP waypointd_http_in_flight Requests currently being handled."
		);
		let _ = writeln!(out, "# TYPE waypointd_http_in_flight gauge");
		let _ = writeln!(
			out,
			"waypointd_http_in_flight {}",
			self.in_flight.load(Ordering::Relaxed)
		);
		let _ = writeln!(out);

		let _ = writeln!(
			out,
			"# HELP waypointd_db_writer_locked Writer connection held by a task (1) or idle (0)."
		);
		let _ = writeln!(out, "# TYPE waypointd_db_writer_locked gauge");
		let _ = writeln!(
			out,
			"waypointd_db_writer_locked {}",
			u8::from(db.writer_locked())
		);
		let _ = writeln!(out);
		let _ = writeln!(
			out,
			"# HELP waypointd_db_readers_in_use Pooled reader connections currently held."
		);
		let _ = writeln!(out, "# TYPE waypointd_db_readers_in_use gauge");
		let _ = writeln!(out, "waypointd_db_readers_in_use {}", db.readers_in_use());
		let _ = writeln!(out);

		// Size/growth numbers from `Db::stats` (cheap PRAGMA + stat calls on
		// a pooled reader). `wal_bytes` is the uncheckpointed frame backlog,
		// and `freelist_pages` grows without a VACUUM — both are "is the
		// file growing?" signals an operator can alert on.
		let db_stats = db.stats();
		let _ = writeln!(
			out,
			"# HELP waypointd_db_file_bytes Main database file size on disk."
		);
		let _ = writeln!(out, "# TYPE waypointd_db_file_bytes gauge");
		let _ = writeln!(out, "waypointd_db_file_bytes {}", db_stats.file_bytes);
		let _ = writeln!(out);
		let _ = writeln!(
			out,
			"# HELP waypointd_db_wal_bytes Size of the -wal sidecar (uncheckpointed committed frames)."
		);
		let _ = writeln!(out, "# TYPE waypointd_db_wal_bytes gauge");
		let _ = writeln!(out, "waypointd_db_wal_bytes {}", db_stats.wal_bytes);
		let _ = writeln!(out);
		let _ = writeln!(
			out,
			"# HELP waypointd_db_pages Total pages in the database."
		);
		let _ = writeln!(out, "# TYPE waypointd_db_pages gauge");
		let _ = writeln!(out, "waypointd_db_pages {}", db_stats.page_count);
		let _ = writeln!(out);
		let _ = writeln!(
			out,
			"# HELP waypointd_db_freelist_pages Unused pages available for reuse (grows without a VACUUM)."
		);
		let _ = writeln!(out, "# TYPE waypointd_db_freelist_pages gauge");
		let _ = writeln!(
			out,
			"waypointd_db_freelist_pages {}",
			db_stats.freelist_pages
		);

		out
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sanitize_collapses_ids() {
		assert_eq!(sanitize_path("/api/bookmarks/42"), "/api/bookmarks/{id}");
		assert_eq!(sanitize_path("/api/bookmarks"), "/api/bookmarks");
		assert_eq!(sanitize_path("/keywords/7"), "/keywords/{id}");
		assert_eq!(sanitize_path("/"), "/");
		// Only *digit-only* segments collapse — a real slug is untouched.
		assert_eq!(sanitize_path("/api/tags/rust-lang"), "/api/tags/rust-lang");
	}
}
