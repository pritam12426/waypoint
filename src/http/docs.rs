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
//! There is no vendored interactive UI: embedding Swagger UI added ~5MB of
//! assets to the release binary, so the UI is served as a small HTML shell
//! that loads swagger-ui-dist from a CDN and points at the spec served
//! here. Offline self-hosters still get the raw JSON for external tooling
//! (curl, code generators); the interactive page needs internet access.

use axum::Json;
use axum::response::Html;
use utoipa::{
	Modify, OpenApi,
	openapi::security::{Http, HttpAuthScheme, SecurityScheme},
};

use crate::http::{error::ApiErrorBody, handlers::*};
use crate::model::*;

/// The interactive API page. It is a minimal HTML shell that loads Swagger
/// UI from a pinned unpkg CDN release and asks it to render the spec from
/// `/api/openapi.json` — the same endpoint `serve_openapi` answers, behind
/// the same auth gate. The shell itself carries no assets, keeping the
/// release binary free of the ~5MB the vendored UI used to add.
pub async fn serve_docs_ui() -> Html<&'static str> {
	crate::log_trace!("GET /api/docs");
	Html(DOCS_UI_HTML)
}

/// Serves the generated OpenAPI document as JSON. Used by the `/api`
/// router's `/api/openapi.json` route (and by the `/api/docs` UI shell).
pub async fn serve_openapi() -> Json<utoipa::openapi::OpenApi> {
	crate::log_trace!("GET /api/openapi.json");
	Json(ApiDoc::openapi())
}

/// The UI shell, pinned to a specific swagger-ui-dist release so a future
/// CDN change can't silently alter the page. Bump the version in the CSS
/// and JS URLs together to pick up a newer release.
const DOCS_UI_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>waypointd API</title>
  <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5.18.2/swagger-ui.css" />
  <style>
    html { box-sizing: border-box; overflow: -moz-scrollbars-vertical; overflow-y: scroll; }
    body { margin: 0; background: #fafafa; }
    .topbar { display: none; }
  </style>
</head>
<body>
  <div id="swagger-ui"></div>
  <script src="https://unpkg.com/swagger-ui-dist@5.18.2/swagger-ui-bundle.js"></script>
  <script>
    window.onload = function () {
      window.ui = SwaggerUIBundle({
        url: "/api/openapi.json",
        dom_id: "#swagger-ui",
        deepLinking: true,
        presets: [SwaggerUIBundle.presets.apis, SwaggerUIBundle.SwaggerUIBundle],
        layout: "StandaloneLayout",
      });
    };
  </script>
</body>
</html>"##;

/// The complete OpenAPI document for the waypoint HTTP API. The spec is
/// served at `/api/openapi.json` (behind the same auth middleware as the
/// API itself when a token is configured). The `/healthz`, `/readyz` and
/// `/metrics` probes and the `/keywords` / `/open` redirects are not part
/// of this document — they are operational and browser-facing surfaces,
/// not API endpoints.
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
		import_bookmarks,
		export_bookmarks,
		start_check,
		check_status,
		check_one_bookmark,
		sign_in,
		sign_out,
		auth_status,
		admin_backup,
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
		SignInRequest,
		SignInResponse,
		AuthStatus,
	)),
	security(("bearer_auth" = [])),
	tags(
		(name = "bookmarks", description = "Bookmark CRUD and lifecycle"),
		(name = "categories", description = "Category listing"),
		(name = "tags", description = "Tag listing"),
		(name = "trash", description = "Recycle bin operations"),
		(name = "search", description = "Full-text search"),
		(name = "stats", description = "Aggregate statistics"),
		(name = "auth", description = "Session sign-in/out and status"),
		(name = "admin", description = "Operator endpoints (backup)"),
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
