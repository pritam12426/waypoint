/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! In-memory registry for long-running background jobs — today that's the
//! dead-link checker (`POST /api/check` starts a job, `GET /api/check/{id}`
//! polls it).
//!
//! Jobs live in process memory: they're transient batch runs, and a crash
//! simply loses them. Finished jobs are reaped after a TTL so the map
//! doesn't grow forever; the registry is shared across handlers via
//! `AppState`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::core::checker::CheckReport;

/// How long a finished job stays queryable before it is reaped.
pub const JOB_TTL: Duration = Duration::from_secs(3600);

/// A numeric job handle. Monotonic within this process, which is all that's
/// needed — a job id is meaningless outside the running server anyway.
pub type JobId = u64;

/// The mutable state of one check job, updated from the worker task as the
/// run progresses.
#[derive(Debug, Clone)]
pub enum CheckJobState {
	Running {
		started_at: SystemTime,
		checked: usize,
		total: usize,
		dead: usize,
	},
	Finished {
		started_at: SystemTime,
		finished_at: SystemTime,
		result: Result<CheckReport, String>,
	},
}

/// A single job handle. `Clone` so handlers can hand one to the worker
/// task and keep one to answer polls with.
#[derive(Clone)]
pub struct CheckJob {
	state: Arc<Mutex<CheckJobState>>,
}

impl CheckJob {
	fn new() -> Self {
		Self {
			state: Arc::new(Mutex::new(CheckJobState::Running {
				started_at: SystemTime::now(),
				checked: 0,
				total: 0,
				dead: 0,
			})),
		}
	}

	/// Replaces the in-flight progress counters. Called from the worker
	/// task (via the checker's progress callback).
	pub fn update_progress(&self, checked: usize, total: usize, dead: usize) {
		let Ok(mut state) = self.state.lock() else {
			return;
		};
		let CheckJobState::Running {
			checked: c,
			total: t,
			dead: d,
			..
		} = &mut *state
		else {
			return;
		};
		*c = checked;
		*t = total;
		*d = dead;
	}

	/// Marks the job finished. Called once by the worker task when `run`
	/// returns or errors.
	pub fn finish(&self, result: Result<CheckReport, String>) {
		if let Ok(mut state) = self.state.lock() {
			let started_at = match &*state {
				CheckJobState::Running { started_at, .. } => *started_at,
				_ => SystemTime::now(),
			};
			*state = CheckJobState::Finished {
				started_at,
				finished_at: SystemTime::now(),
				result,
			};
		}
	}

	/// A point-in-time copy of the job's state, for answering a poll.
	pub fn snapshot(&self) -> CheckJobState {
		self.state.lock().unwrap().clone()
	}
}

/// The registry itself: id assignment, lookup, and TTL reaping.
#[derive(Default)]
pub struct Jobs {
	next_id: AtomicU64,
	jobs: Mutex<HashMap<JobId, CheckJob>>,
}

impl Jobs {
	pub fn new() -> Self {
		Self::default()
	}

	/// Registers a fresh running job and returns its id.
	pub fn start(&self) -> (JobId, CheckJob) {
		let job = CheckJob::new();
		let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
		self.jobs.lock().unwrap().insert(id, job.clone());
		(id, job)
	}

	/// Looks up a job, reaping anything finished past its TTL first so a
	/// poll doesn't return a long-dead handle.
	pub fn get(&self, id: JobId) -> Option<CheckJob> {
		let mut jobs = self.jobs.lock().unwrap();
		self.reap_locked(&mut jobs, SystemTime::now());
		jobs.get(&id).cloned()
	}

	/// Drops finished jobs older than `JOB_TTL`. Cheap enough to run on
	/// every poll; also called after a job finishes to keep the map small.
	pub fn reap(&self) {
		let mut jobs = self.jobs.lock().unwrap();
		self.reap_locked(&mut jobs, SystemTime::now());
	}

	fn reap_locked(&self, jobs: &mut HashMap<JobId, CheckJob>, now: SystemTime) {
		jobs.retain(|_, job| {
			let Ok(state) = job.state.lock() else {
				return true;
			};
			match &*state {
				CheckJobState::Running { .. } => true,
				CheckJobState::Finished { finished_at, .. } => {
					now.duration_since(*finished_at).unwrap_or(Duration::ZERO) < JOB_TTL
				}
			}
		});
	}
}
