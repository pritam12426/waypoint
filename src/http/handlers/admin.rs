/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Admin endpoints. Today that's just the manual `VACUUM INTO` backup
//! snapshot.

use axum::{Json, extract::State};
use serde::Serialize;
use utoipa::ToSchema;

use crate::http::{
	AppState,
	error::{ApiErrorBody, AppError},
};

// ============================================================
// Admin endpoints
// ============================================================

/// Result of a manual backup.
#[derive(Serialize, ToSchema)]
pub struct BackupResult {
	/// Absolute path of the written backup file.
	pub path: String,
	/// How many older automated backups were pruned (kept: `BACKUP_KEEP`).
	pub pruned: usize,
	/// Local timestamp of the snapshot.
	pub created_at: String,
}

/// Runs a `VACUUM INTO` backup snapshot immediately, in addition to the
/// scheduled ones. Requires `WAYPOINTD_BACKUP_DIR` to be configured (that's
/// where the snapshot is written); responds 400 otherwise.
#[utoipa::path(
	post,
	path = "/api/admin/backup",
	tag = "admin",
	responses(
		(status = 201, description = "Backup written", body = BackupResult),
		(status = 400, description = "No backup directory configured", body = ApiErrorBody),
	)
)]
pub async fn admin_backup(State(state): State<AppState>) -> Result<Json<BackupResult>, AppError> {
	let Some(backup) = &state.backup else {
		return Err(AppError::invalid_payload(
			"manual backups are disabled: start the server with WAYPOINTD_BACKUP_DIR \
			 to point at a backup folder",
		));
	};
	let now = chrono::Local::now();
	let dest = backup.dir.join(crate::database::backup_filename(&now));
	let dest_display = dest.display().to_string();
	let db = state.db.clone();
	let backup_dest = dest.clone();
	tokio::task::spawn_blocking(move || db.backup(&backup_dest))
		.await
		.map_err(|_| AppError::internal())??;
	let dir = backup.dir.clone();
	let keep = backup.keep;
	let pruned = tokio::task::spawn_blocking(move || crate::database::prune_backups(&dir, keep))
		.await
		.unwrap_or(0);
	crate::log_info!("manual backup written to {dest_display}");
	Ok(Json(BackupResult {
		path: dest_display,
		pruned,
		created_at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
	}))
}
