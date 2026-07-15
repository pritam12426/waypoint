/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Unified error type for the HTTP layer. Every handler returns
//! `Result<_, AppError>`; this module defines the stable machine-readable
//! error codes, maps SQLite failures onto client-facing errors, and renders
//! everything as the `{"error", "code"}` JSON contract the frontend and
//! docs rely on.

use axum::{
	Json,
	http::{HeaderName, StatusCode},
	response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

/// Response header carrying the machine-readable error code. Set by
/// `AppError::into_response`; `http::log_failures` reads it to skip
/// responses that already got a code+message log line, and clients can
/// read the code without parsing the body.
pub(crate) const X_WAYPOINT_ERROR: HeaderName = HeaderName::from_static("x-waypoint-error");

/// The JSON body every error response carries: a human-readable `message`
/// plus a stable machine-readable `code` clients can switch on.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiErrorBody {
	pub error: String,
	pub code: String,
}

/// Stable machine-readable codes. Kept as an enum so the string can never
/// drift from the list of documented values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
	InvalidUrl,
	InvalidKeyword,
	InvalidLimit,
	InvalidOffset,
	InvalidId,
	InvalidName,
	InvalidDate,
	InvalidPayload,
	QueryRequired,
	NotFound,
	ConflictUrl,
	ConflictKeyword,
	Unauthorized,
	Forbidden,
	Busy,
	Timeout,
	ReplayConflict,
	Internal,
}

impl ErrorCode {
	pub fn as_str(self) -> &'static str {
		match self {
			ErrorCode::InvalidUrl => "invalid_url",
			ErrorCode::InvalidKeyword => "invalid_keyword",
			ErrorCode::InvalidLimit => "invalid_limit",
			ErrorCode::InvalidOffset => "invalid_offset",
			ErrorCode::InvalidId => "invalid_id",
			ErrorCode::InvalidName => "invalid_name",
			ErrorCode::InvalidDate => "invalid_date",
			ErrorCode::InvalidPayload => "invalid_payload",
			ErrorCode::QueryRequired => "query_required",
			ErrorCode::NotFound => "not_found",
			ErrorCode::ConflictUrl => "conflict_url",
			ErrorCode::ConflictKeyword => "conflict_keyword",
			ErrorCode::Unauthorized => "unauthorized",
			ErrorCode::Forbidden => "forbidden",
			ErrorCode::Busy => "busy",
			ErrorCode::Timeout => "request_timeout",
			ErrorCode::ReplayConflict => "idempotency_conflict",
			ErrorCode::Internal => "internal_error",
		}
	}

	pub fn status(self) -> StatusCode {
		match self {
			ErrorCode::InvalidUrl
			| ErrorCode::InvalidKeyword
			| ErrorCode::InvalidLimit
			| ErrorCode::InvalidOffset
			| ErrorCode::InvalidId
			| ErrorCode::InvalidName
			| ErrorCode::InvalidDate
			| ErrorCode::InvalidPayload
			| ErrorCode::QueryRequired => StatusCode::BAD_REQUEST,
			ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
			ErrorCode::Forbidden => StatusCode::FORBIDDEN,
			ErrorCode::NotFound => StatusCode::NOT_FOUND,
			ErrorCode::ConflictUrl | ErrorCode::ConflictKeyword => StatusCode::CONFLICT,
			ErrorCode::Busy => StatusCode::SERVICE_UNAVAILABLE,
			ErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
			ErrorCode::ReplayConflict => StatusCode::CONFLICT,
			ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
		}
	}
}

/// Unified error type for every handler. Construct it with the helper
/// constructors, or let `?` convert any error via `From<E>`.
