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
#[derive(Debug)]
pub struct AppError {
	code: ErrorCode,
	message: String,
	/// Whether the caller already logged this rejection. Handlers that
	/// emit their own (context-rich) log line — e.g. a failed sign-in with
	/// the client address — can mark the error so `into_response` doesn't
	/// log the same failure a second time. The `x-waypoint-error` header
	/// is still set either way, so the middleware keeps skipping it too.
	already_logged: bool,
}

impl AppError {
	pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
		Self {
			code,
			message: message.into(),
			already_logged: false,
		}
	}

	/// Marks this error as already logged, suppressing the generic
	/// `request rejected (..)` line `into_response` would otherwise emit.
	/// Use when the handler produced a more specific log line itself.
	pub fn already_logged(mut self) -> Self {
		self.already_logged = true;
		self
	}

	pub fn invalid_url(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::InvalidUrl, message)
	}

	pub fn invalid_keyword(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::InvalidKeyword, message)
	}

	pub fn invalid_limit(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::InvalidLimit, message)
	}

	pub fn invalid_offset(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::InvalidOffset, message)
	}

	pub fn invalid_id(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::InvalidId, message)
	}

	pub fn invalid_name(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::InvalidName, message)
	}

	pub fn invalid_date(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::InvalidDate, message)
	}

	pub fn invalid_payload(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::InvalidPayload, message)
	}

	pub fn query_required(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::QueryRequired, message)
	}

	pub fn not_found(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::NotFound, message)
	}

	pub fn conflict_url(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::ConflictUrl, message)
	}

	pub fn conflict_keyword(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::ConflictKeyword, message)
	}

	pub fn unauthorized(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::Unauthorized, message)
	}

	pub fn forbidden(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::Forbidden, message)
	}

	pub fn busy(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::Busy, message)
	}

	pub fn timeout(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::Timeout, message)
	}

	pub fn replay_conflict(message: impl Into<String>) -> Self {
		Self::new(ErrorCode::ReplayConflict, message)
	}

	pub fn internal() -> Self {
		Self::new(ErrorCode::Internal, "internal server error")
	}
}

impl IntoResponse for AppError {
	fn into_response(self) -> Response {
		let status = self.code.status();
		// Server-side and client-side failures are logged at different
		// severities: 500s are bugs the operator needs to know about, 4xxs
		// are routine client mistakes. Skip the line entirely when the
		// handler already logged a more specific version of the rejection.
		if !self.already_logged && status == StatusCode::INTERNAL_SERVER_ERROR {
			crate::log_error!("request failed ({}): {}", self.code.as_str(), self.message);
		} else if !self.already_logged {
			crate::log_warn!(
				"request rejected ({}): {}",
				self.code.as_str(),
				self.message
			);
		}

		let body = Json(ApiErrorBody {
			error: self.message,
			code: self.code.as_str().into(),
		});
		let mut res = (status, body).into_response();
		// The error code as a header so `http::log_failures` can tell this
		// (already-logged) failure apart from unlogged ones.
		res.headers_mut()
			.insert(X_WAYPOINT_ERROR, self.code.as_str().parse().unwrap());
		// RFC 7235: a 401 must carry a `WWW-Authenticate` challenge so
		// clients know which scheme to use. The frontend's `fetch` reads it
		// to decide whether to prompt for a token.
		if self.code == ErrorCode::Unauthorized {
			res.headers_mut()
				.insert("WWW-Authenticate", "Bearer".parse().unwrap());
		}
		res
	}
}

/// Blanket conversion so handlers can use `?` on `anyhow::Result`,
/// `rusqlite::Result`, `spawn_blocking` joins, etc. Known client-side
/// SQLite failures are classified into the right 4xx code; everything
/// else is logged with its full context chain and turned into a generic
/// 500 (so internal details never leak to API consumers).
impl<E> From<E> for AppError
where
	E: Into<anyhow::Error>,
{
	fn from(err: E) -> Self {
		let any = err.into();
		// Friendly duplicate detection in `database::bookmarks::insert`
		// pre-checks the URL before the INSERT, so its duplicate error is a
		// plain anyhow message rather than a SQLite constraint failure.
		// Classify it here so the HTTP contract (409 conflict_url) holds.
		if any.to_string().contains("already exists as bookmark") {
			return AppError::conflict_url(any.to_string());
		}
		// The keyword twin: `insert`/`update` pre-check the shortcut the
		// same way, so the 409 conflict_keyword message is friendly too.
		if any.to_string().contains("already in use by bookmark") {
			return AppError::conflict_keyword(any.to_string());
		}
		// `database::bookmarks` refuses to store a redirect template that
		// has no keyword shortcut to trigger it — surface that as a friendly
		// 400 invalid_payload, not a 500. Keep the message in sync with the
		// DB-side guard.
		if any.to_string().contains("requires a keyword") {
			return AppError::invalid_payload(any.to_string());
		}
		// Anything that surfaces as a SQLite failure (unique constraint on
		// update, etc.) goes through `classify_sqlite`; the guard `let`
		// chain means we only take the classification when it says "this is
		// a client error".
		if let Some(db_err) = any.downcast_ref::<rusqlite::Error>()
			&& let Some(app_err) = classify_sqlite(db_err)
		{
			return app_err;
		}
		// Genuinely unexpected: log the full context chain (with {:#}) for
		// the operator, but tell the client only "internal server error".
		crate::log_error!("internal error: {any:#}");
		AppError::internal()
	}
}

/// Maps known SQLite failures to client-facing errors. Returns `None` for
/// anything that is genuinely a server problem (locked DB, malformed
/// image, I/O, ...).
fn classify_sqlite(err: &rusqlite::Error) -> Option<AppError> {
	let rusqlite::Error::SqliteFailure(failure, _) = err else {
		return None;
	};
	// SQLITE_CONSTRAINT_UNIQUE — a duplicate URL or keyword. Which one the
	// message names decides the code; `bookmarks.url` and `bookmarks.keyword`
	// are the only two unique constraints on the table.
	if failure.extended_code != 2067 {
		return None;
	}
	let msg = err.to_string();
	if msg.contains("bookmarks.keyword") {
		Some(AppError::conflict_keyword(msg))
	} else {
		Some(AppError::conflict_url(msg))
	}
}
