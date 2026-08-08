/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! OpenAPI 3.0 document for the waypoint HTTP API, generated at build time
//! by `utoipa` from the handler signatures and model schemas, then served
//! as JSON at `/api/openapi.json`. Adding an endpoint means adding the
//! handler to the `paths(...)` list here (and usually the
//! `components(schemas(...))` list too) — the compiler keeps this honest.
//!
//! There is no interactive Swagger UI: the vendored UI embedded ~5MB of
//! assets into the release binary, so it was dropped. The raw spec remains
//! for external tooling (curl, code generators).

use axum::Json;
use utoipa::{
	Modify, OpenApi,
	openapi::security::{Http, HttpAuthScheme, SecurityScheme},
};

use crate::http::{error::ApiErrorBody, handlers::*};
use crate::model::*;

/// Serves the generated OpenAPI document as JSON. Used by the `/api`
/// router's `/api/openapi.json` route.
pub async fn serve_openapi() -> Json<utoipa::openapi::OpenApi> {
	Json(ApiDoc::openapi())
}

/// The complete OpenAPI document for the waypoint HTTP API. The spec is
/// served at `/api/openapi.json` (behind the same bearer-token middleware
/// as the API itself when one is configured).
#[derive(OpenApi)]
#[openapi(
	paths(
		list_bookmarks,
		create_bookmark,
		get_bookmark,
		update_bookmark,
		delete_bookmark,
		bulk_delete_bookmarks,
		bulk_update_bookmarks,
		restore_bookmark,
		empty_trash,
		list_categories,
		rename_category,
		delete_category,
		list_tags,
		rename_tag,
		delete_tag,
		search_bookmarks,
		stats_overview,
		domain_stats,
		stats_tags,
		stats_bookmark_detail,
		stats_top_visited,
		stats_never_visited,
		stats_orphan_tags,
		stats_hygiene,
		stats_activity,
		keyword_list,
		keyword_redirect,
		open_bookmark,
	),
	components(schemas(
		Bookmark,
		NewBookmark,
		UpdateBookmark,
		Category,
		TagCount,
		DomainCount,
		CategoryCount,
		BookmarkVisitStats,
		StatsOverview,
		DomainVisitStats,
		NeverVisitedBookmark,
		OrphanTag,
		HygieneStats,
		MonthlyActivity,
		ApiErrorBody,
		BulkRemoveResult,
		BulkUpdateRequest,
		BulkUpdateResult,
	)),
	security(("bearer_auth" = [])),
	tags(
		(name = "bookmarks", description = "Bookmark CRUD and lifecycle"),
		(name = "categories", description = "Category listing"),
		(name = "tags", description = "Tag listing"),
		(name = "trash", description = "Recycle bin operations"),
		(name = "search", description = "Full-text search"),
		(name = "stats", description = "Aggregate statistics"),
		(name = "keywords", description = "Keyword shortcut redirects (no auth)"),
	),
	modifiers(&SecurityAddon),
)]
pub struct ApiDoc;

/// Declares the optional bearer-token security scheme. It's optional in the
/// docs because the token only exists when the operator passes `--api-token`;
/// unauthenticated servers still honor the same OpenAPI document.
struct SecurityAddon;

impl Modify for SecurityAddon {
	fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
		if let Some(components) = openapi.components.as_mut() {
			components.add_security_scheme(
				"bearer_auth",
				SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
			);
		}
	}
}
