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

