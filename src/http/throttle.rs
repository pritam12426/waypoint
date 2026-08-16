/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Per-client sign-in throttling.
//!
//! Auth tokens are long random strings, so brute-forcing one isn't the
//! threat this breaks; the real case is a token that leaked and gets used
//! from the wrong place, or a scripted spray against the sign-in endpoint.
//! The breaker is per-IP: too many failed exchanges within a window locks
//! the IP out for a cooldown, and a successful sign-in resets the count.
//!
//! Storage is in-process memory — the server is a single self-hosted
//! process, a reboot dropping a lockout is acceptable (the window is a
//! spray-brake, not a fortress), and per-IP tracking lives naturally in
//! `AppState` alongside the other shared maps.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Failed exchanges per window before an IP is locked out.
const MAX_ATTEMPTS: u32 = 5;
/// How long a run of failures is counted (and the resulting lockout
/// duration). A cooldown roughly as long as the count window keeps the
/// spray rate capped at ~one guess per cooldown.
const WINDOW: Duration = Duration::from_secs(15 * 60);

struct Attempts {
	window_start: Instant,
	count: u32,
	locked_until: Option<Instant>,
}

/// In-memory per-IP failure tracker for `POST /api/auth/signin`.
#[derive(Default)]
pub struct LoginThrottle {
	inner: Mutex<HashMap<IpAddr, Attempts>>,
}

impl LoginThrottle {
	pub fn new() -> Self {
		Self::default()
	}

	/// Whether `ip` is currently locked out.
	pub fn locked(&self, ip: IpAddr) -> bool {
		let now = Instant::now();
		let mut inner = self.inner.lock().unwrap();
		prune(&mut inner, now);
		inner
			.get(&ip)
			.and_then(|a| a.locked_until)
			.is_some_and(|l| l > now)
	}

	/// Records a failed exchange. Returns true the moment the IP crosses
	/// into lockout (so the caller can log it).
	pub fn record_failure(&self, ip: IpAddr) -> bool {
		let now = Instant::now();
		let mut inner = self.inner.lock().unwrap();
		prune(&mut inner, now);
		let attempts = inner.entry(ip).or_insert(Attempts {
			window_start: now,
			count: 0,
			locked_until: None,
		});
		// A fresh window after the lockout lapses restarts the count.
		if attempts.locked_until.is_none() && now.duration_since(attempts.window_start) >= WINDOW {
			attempts.window_start = now;
			attempts.count = 0;
		}
		attempts.count += 1;
		if attempts.count >= MAX_ATTEMPTS {
			attempts.locked_until = Some(now + WINDOW);
			true
		} else {
			false
		}
	}

	/// Clears the failure record after a successful sign-in.
	pub fn record_success(&self, ip: IpAddr) {
		self.inner.lock().unwrap().remove(&ip);
	}
}

/// Drops entries that are neither inside their count window nor currently
/// locked out, so a stream of distinct IPs can't grow the map unbounded.
fn prune(inner: &mut HashMap<IpAddr, Attempts>, now: Instant) {
	inner.retain(|_, a| {
		let locked = a.locked_until.is_some_and(|l| l > now);
		let in_window = a.locked_until.is_none() && now.duration_since(a.window_start) < WINDOW;
		locked || in_window
	});
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn locks_out_after_max_attempts_and_resets() {
		let t = LoginThrottle::new();
		let ip: IpAddr = "127.0.0.1".parse().unwrap();
		for _ in 0..MAX_ATTEMPTS - 1 {
			assert!(!t.record_failure(ip));
			assert!(!t.locked(ip));
		}
		assert!(t.record_failure(ip));
		assert!(t.locked(ip));
		t.record_success(ip);
		assert!(!t.locked(ip));
	}

	#[test]
	fn distinct_ips_are_independent() {
		let t = LoginThrottle::new();
		let a: IpAddr = "10.0.0.1".parse().unwrap();
		let b: IpAddr = "10.0.0.2".parse().unwrap();
		for _ in 0..MAX_ATTEMPTS {
			t.record_failure(a);
		}
		assert!(t.locked(a));
		assert!(!t.locked(b));
	}
}
