/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Dead-link check endpoints: the background batch job (`POST /api/check`
//! starts it, `GET /api/check/{id}` polls it) and the synchronous
//! single-link check (`GET /api/bookmarks/{id}/check`).

use std::net::SocketAddr;

use axum::{
	Json,
	extract::{ConnectInfo, Path, State},
	http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::http::error::{ApiErrorBody, AppError};
use crate::{
	core::checker,
	http::{AppState, jobs::CheckJobState},
};

/// Request body for `POST /api/check`.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckRequest {
	/// Move dead links to the trash (recoverable).
	#[serde(default)]
	delete: bool,
	/// Permanently purge dead links.
	#[serde(default)]
	hard_delete: bool,
	/// Worker threads for the probe pool. Clamped to at least 1; defaults
	/// to the number of CPUs.
	jobs: Option<usize>,
}

/// Response to `POST /api/check`: the job id to poll.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckStarted {
	id: u64,
}

/// A pollable snapshot of a check job.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum CheckStatus {
	/// The job is still probing bookmarks.
	Running {
		checked: usize,
		total: usize,
		dead: usize,
	},
	/// The job completed with a report.
	Finished {
		checked: usize,
		alive: usize,
		skipped: usize,
		deleted: usize,
		dead: Vec<checker::DeadLink>,
	},
	/// The job failed before producing a report.
	Failed { error: String },
}

/// Starts a dead-link check as a background job. Returns `202` with the job
/// id immediately; poll `GET /api/check/{id}` for progress and the final
/// report.
#[utoipa::path(
	post,
	path = "/api/check",
	tag = "bookmarks",
	request_body = CheckRequest,
	responses(
		(
			status = 202,
			description = "Check started",
			body = CheckStarted,
		),
		(status = 400, description = "delete and hardDelete are mutually exclusive", body = ApiErrorBody),
	)
)]
pub async fn start_check(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Json(body): Json<CheckRequest>,
) -> Result<(StatusCode, Json<CheckStarted>), AppError> {
	if body.delete && body.hard_delete {
		return Err(AppError::invalid_payload(
			"delete and hardDelete are mutually exclusive; pick one",
		));
	}
	let jobs = body.jobs.unwrap_or_else(default_check_jobs).max(1);
	crate::log_debug!(
		"{addr} POST /api/check (delete={}, hard_delete={}, jobs={jobs})",
		body.delete,
		body.hard_delete
	);

	let (job_id, job) = state.jobs.start();
	let jobs_registry = state.jobs.clone();
	let db = state.db.clone();
	let delete = body.delete;
	let hard_delete = body.hard_delete;

	tokio::task::spawn_blocking(move || {
		// A check that only reports reads; one that deletes/purges needs the
		// writer. Either way the connection is only touched on this thread.
		let progress_job = job.clone();
		let result = if delete || hard_delete {
			let conn = db.writer();
			checker::run(&conn, delete, hard_delete, jobs, move |p| {
				progress_job.update_progress(p.checked, p.total, p.dead);
			})
		} else {
			let conn = db.reader();
			checker::run(&conn, false, false, jobs, move |p| {
				progress_job.update_progress(p.checked, p.total, p.dead);
			})
		};
		job.finish(result.map_err(|e| format!("{e:#}")));
		jobs_registry.reap();
	});

	Ok((StatusCode::ACCEPTED, Json(CheckStarted { id: job_id })))
}

/// Returns a check job's current status. While the job runs this carries
/// cumulative progress; once it's done it carries the full report.
#[utoipa::path(
	get,
	path = "/api/check/{id}",
	tag = "bookmarks",
	params(("id" = u64, Path, description = "Check job id")),
	responses(
		(
			status = 200,
			description = "Job status",
			body = CheckStatus,
		),
		(status = 404, description = "No such check job", body = ApiErrorBody),
	)
)]
pub async fn check_status(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<u64>,
) -> Result<Json<CheckStatus>, AppError> {
	let Some(job) = state.jobs.get(id) else {
		return Err(AppError::not_found(format!(
			"no check job with id {id} (it may have finished long ago and been reaped)"
		)));
	};
	let status = match job.snapshot() {
		CheckJobState::Running {
			checked,
			total,
			dead,
			..
		} => CheckStatus::Running {
			checked,
			total,
			dead,
		},
		CheckJobState::Finished { result, .. } => match result {
			Ok(report) => CheckStatus::Finished {
				checked: report.checked,
				alive: report.alive,
				skipped: report.skipped,
				deleted: report.deleted,
				dead: report.dead,
			},
			Err(error) => CheckStatus::Failed { error },
		},
	};
	crate::log_debug!("{addr} GET /api/check/{id}");
	Ok(Json(status))
}

/// Checks a single active bookmark by id and returns whether it's alive.
/// Unlike `POST /api/check` this is synchronous — one URL, one probe — so
/// there's no job to poll.
#[utoipa::path(
	get,
	path = "/api/bookmarks/{id}/check",
	tag = "bookmarks",
	params(("id" = i64, Path, description = "Bookmark id")),
	responses(
		(
			status = 200,
			description = "Liveness verdict",
			body = checker::CheckVerdict,
		),
		(status = 404, description = "No such active bookmark", body = ApiErrorBody),
	)
)]
pub async fn check_one_bookmark(
	State(state): State<AppState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Path(id): Path<i64>,
) -> Result<Json<checker::CheckVerdict>, AppError> {
	crate::log_debug!("{addr} GET /api/bookmarks/{id}/check");
	let db = state.db.clone();
	let verdict =
		tokio::task::spawn_blocking(move || checker::check_one(&db.reader(), id)).await??;
	match verdict {
		Some(v) => Ok(Json(v)),
		None => Err(AppError::not_found(format!(
			"no active bookmark #{id} to check (it may be trashed)"
		))),
	}
}

/// Default worker-pool size for a check run: the number of CPUs, at least 1.
fn default_check_jobs() -> usize {
	std::thread::available_parallelism()
		.map(|n| n.get())
		.unwrap_or(4)
		.max(1)
}
