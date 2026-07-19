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
