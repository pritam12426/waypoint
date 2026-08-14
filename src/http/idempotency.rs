/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! `Idempotency-Key` support for the mutating endpoints.
//!
//! The waypoint API has no client-visible transaction ids, so a client that
//! retries a create after a timeout can't tell whether the bookmark was
//! actually inserted (the duplicate check would 409, which is indistinguishable
//! from a genuinely-colliding save). The `Idempotency-Key` header fixes that
//! the standard way (RFC-style, like Stripe):
//!
//! * the client sends a unique key with a mutating request;
//! * the server remembers the first request's response for that key;
//! * a retry with the *same* key and *same* payload is answered with the
//!   stored response instead of being executed again;
//! * a retry with the same key but a *different* payload is rejected with
//!   409 `idempotency_conflict` — the key has already been used for
//!   something else.
//!
//! Keys expire after [`TTL`], so a key can be reused for a genuinely new
//! request once the original is long past any conceivable retry window.
//! Storage is in-process memory: the server is a single self-hosted process,
//! and idempotency is a short-window safety net, not a durable ledger.

use axum::{
	body::{Body, to_bytes},
	extract::{Request, State},
	http::{StatusCode, header},
	middleware::Next,
	response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{AppState, error::AppError};

/// How long a stored idempotent response is honored.
const TTL: Duration = Duration::from_secs(10 * 60);

/// Longest accepted key. Anything longer is rejected rather than stored —
/// keys are client-chosen, so this caps memory from a misbehaving client.
const MAX_KEY_LEN: usize = 64;

/// A remembered request/response pair for one key.
struct Entry {
	/// When the original request ran — used for expiry.
	at: Instant,
	/// Hash of (method, path, body). A retry with a different hash means the
	/// key was reused for a different request → 409.
	request_hash: u64,
	/// The status + body of the original response, replayed verbatim.
	status: StatusCode,
	body: Vec<u8>,
}

/// In-process idempotency ledger. See the module docs for the semantics.
#[derive(Default)]
pub struct IdempotencyStore {
	inner: Mutex<HashMap<String, Entry>>,
}

impl IdempotencyStore {
	pub fn new() -> Self {
		Self::default()
	}

	/// Looks up a stored entry, pruning expired ones lazily (a hit under a
	/// saturated store is rare, so an occasional expired entry is not worth
	/// a periodic sweep).
	fn get(&self, key: &str) -> Option<Entry> {
		let mut inner = self.inner.lock().unwrap();
		let entry = inner.get(key)?;
		if entry.at.elapsed() > TTL {
			inner.remove(key);
			return None;
		}
		Some(Entry {
			at: entry.at,
			request_hash: entry.request_hash,
			status: entry.status,
			body: entry.body.clone(),
		})
	}

	fn put(&self, key: &str, request_hash: u64, status: StatusCode, body: Vec<u8>) {
		self.inner.lock().unwrap().insert(
			key.to_owned(),
			Entry {
				at: Instant::now(),
				request_hash,
				status,
				body,
			},
		);
	}
}

/// Stable fingerprint of what the request means: method + path + body. Two
/// requests with the same key must have the same fingerprint to share an
/// idempotent replay.
fn request_hash(method: &str, path: &str, body: &[u8]) -> u64 {
	let mut hasher = DefaultHasher::new();
	method.hash(&mut hasher);
	path.hash(&mut hasher);
	body.hash(&mut hasher);
	hasher.finish()
}

/// The middleware: active only when the request carries an `Idempotency-Key`
/// header on a mutating method (POST/PATCH/DELETE). GET/HEAD requests pass
/// through untouched even with a header — idempotency only makes sense for
/// requests that *change* state.
pub async fn idempotency(State(state): State<AppState>, req: Request, next: Next) -> Response {
	let Some(key) = req
		.headers()
		.get("idempotency-key")
		.and_then(|v| v.to_str().ok())
		.map(str::trim)
		.filter(|k| !k.is_empty())
		.map(str::to_owned)
	else {
		return next.run(req).await;
	};
	if !matches!(
		req.method(),
		&axum::http::Method::POST | &axum::http::Method::PATCH | &axum::http::Method::DELETE
	) {
		return next.run(req).await;
	}
	if key.len() > MAX_KEY_LEN {
		let err = AppError::invalid_payload(format!(
			"the idempotency-key header must be at most {MAX_KEY_LEN} characters"
		));
		return err.into_response();
	}

	// Buffer the body so it can be hashed *and* passed to the handler.
	let (parts, body) = req.into_parts();
	let body_bytes = match to_bytes(body, 8 * 1024 * 1024).await {
		Ok(bytes) => bytes,
		Err(err) => {
			let app_err = AppError::new(
				crate::http::error::ErrorCode::InvalidPayload,
				format!("could not read request body: {err}"),
			);
			return app_err.into_response();
		}
	};
	let hash = request_hash(parts.method.as_str(), parts.uri.path(), &body_bytes);

	// A replayed request with a matching fingerprint returns the stored
	// response; a mismatched fingerprint is an abuse of the key.
	if let Some(entry) = state.idempotency.get(&key) {
		if entry.request_hash == hash {
			crate::log_trace!("idempotency: replaying stored response for key {key:?}");
			// Every stored response is the JSON body of a mutating endpoint;
			// the original Content-Type is re-applied so the replay parses
			// exactly like the first response.
			let replay = Response::builder()
				.status(entry.status)
				.header(header::CONTENT_TYPE, "application/json")
				.body(Body::from(entry.body))
				.unwrap();
			return replay;
		}
		crate::log_warn!("idempotency: key {key:?} reused for a different request");
		let err = AppError::replay_conflict(format!(
			"the idempotency-key {key:?} was already used for a different request; \
			 use a fresh key per request"
		));
		return err.into_response();
	}

	let request = Request::from_parts(parts, Body::from(body_bytes));
	let response = next.run(request).await;

	// Remember the outcome so a retry replays it — including error statuses
	// (a duplicate-URL 409 is exactly the response a retry should get).
	let status = response.status();
	let (resp_parts, resp_body) = response.into_parts();
	let body = match to_bytes(resp_body, 8 * 1024 * 1024).await {
		Ok(bytes) => bytes.to_vec(),
		Err(_) => Vec::new(),
	};
	state.idempotency.put(&key, hash, status, body.clone());
	Response::from_parts(resp_parts, Body::from(body))
}
