/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Operational probes and metrics. Unauthenticated on purpose: healthz,
//! readyz, and the Prometheus scrape endpoint must answer before auth (or
//! credentials) exist.

use axum::{
	body::Body,
	extract::State,
	http::{StatusCode, header},
	response::{IntoResponse, Response},
};

use crate::http::AppState;

// ============================================================
// Operational probes + metrics (no auth)
// ============================================================

/// Liveness probe: the process is up and the router is answering. This tells
/// a supervisor "the server is running" — it deliberately does *not* touch
/// the database, so a wedged pool doesn't make a healthy-but-busy server
/// look dead (readiness is what checks the DB).
pub async fn healthz() -> Response {
	(StatusCode::OK, "ok").into_response()
}

/// Readiness probe: the server can actually serve a request end-to-end,
/// which here means the SQLite pool can be read. A load balancer or
/// supervisor takes a 503 as "stop sending traffic" (used by the systemd
/// healthcheck, which wants to see a fully-servable server before marking
/// the unit healthy).
pub async fn readyz(State(state): State<AppState>) -> Response {
	let db = state.db.clone();
	let ok = tokio::task::spawn_blocking(move || {
		let conn = db.reader();
		conn.query_row("SELECT 1", [], |_| Ok(()))
	})
	.await;
	match ok {
		Ok(Ok(())) => (StatusCode::OK, "ok").into_response(),
		_ => {
			crate::log_error!("readyz: database not reachable");
			(StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response()
		}
	}
}

/// Prometheus text-format metrics (RED + SQLite pool gauges). Scraped by
/// Prometheus/Grafana agent/VictoriaMetrics; see `http::metrics`.
pub async fn metrics(State(state): State<AppState>) -> Response {
	let body = state.metrics.render(&state.db);
	Response::builder()
		.status(StatusCode::OK)
		.header(header::CONTENT_TYPE, "text/plain; version=0.0.4")
		.body(Body::from(body))
		.expect("metrics response is always valid")
		.into_response()
}
