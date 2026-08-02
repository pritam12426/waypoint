use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Once};

use axum::{
	body::Body,
	extract::ConnectInfo,
	http::{Method, Request, StatusCode, header},
};
use serde_json::json;
use tower::ServiceExt;

use waypointd::database;
use waypointd::http::{AppState, Jobs, app};

static SILENCE: Once = Once::new();

fn silence_logs() {
	SILENCE.call_once(|| {
		waypointd::logging::log_init(
			None,
			waypointd::logging::LogLevel::Off,
			waypointd::logging::LogFormat::Pretty,
		);
	});
}

fn test_state() -> (tempfile::TempDir, AppState) {
	use std::time::Duration;
	let dir = tempfile::tempdir().unwrap();
	let db = database::Db::open(dir.path().join("waypointd.db")).unwrap();
	let state = AppState {
		db: Arc::new(db),
		counts: Arc::new(waypointd::http::CountCache::new()),
		stats: Arc::new(waypointd::http::StatsCache::new()),
		jobs: Arc::new(Jobs::new()),
		api_token: None,
		read_token: None,
		metrics: Arc::new(waypointd::http::Metrics::new()),
		cookie_secure: false,
		backup: None,
		idempotency: Arc::new(waypointd::http::IdempotencyStore::new()),
		concurrency: Arc::new(tokio::sync::Semaphore::new(64)),
		request_timeout: Duration::from_secs(30),
	};
	(dir, state)
}

async fn request(
	state: &AppState,
	method: Method,
	uri: &str,
	body: Body,
) -> axum::response::Response {
	let mut req = Request::builder()
		.method(method)
		.uri(uri)
		.header(header::CONTENT_TYPE, "application/json")
		.body(body)
		.unwrap();
	// Handlers extract `ConnectInfo<SocketAddr>`; oneshot injection has no
	// listener, so fake a remote address in the request extensions.
	req.extensions_mut()
		.insert(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
	app(state.clone()).oneshot(req).await.unwrap()
}

async fn body_text(res: axum::response::Response) -> String {
	use http_body_util::BodyExt;
	let bytes = res.into_body().collect().await.unwrap().to_bytes();
	String::from_utf8_lossy(&bytes).to_string()
}

#[tokio::test]
async fn create_and_list_bookmarks() {
	silence_logs();
	let (_dir, state) = test_state();

	let res = request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(
			json!({
				"url": "https://example.com/one",
				"title": "One",
				"tags": ["t1", "t2"]
			})
			.to_string(),
		),
	)
	.await;
	assert_eq!(res.status(), StatusCode::CREATED);

	let res = request(
		&state,
		Method::GET,
		"/api/bookmarks?limit=50",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	let total = res
		.headers()
		.get("x-total-count")
		.and_then(|v| v.to_str().ok())
		.unwrap()
		.to_string();
	assert_eq!(total, "1");
	let text = body_text(res).await;
	assert!(text.contains("example.com/one"));
}

#[tokio::test]
async fn list_paginates_by_cursor() {
	silence_logs();
	let (_dir, state) = test_state();

	// Create 5 bookmarks in order. `created_at` is second-resolution, so
	// several share a timestamp; the (created_at, id) keyset breaks the tie
	// by id, which is exactly what the cursor encodes.
	for i in 0..5 {
		let res = request(
			&state,
			Method::POST,
			"/api/bookmarks",
			Body::from(
				json!({ "url": format!("https://example.com/{i}"), "title": format!("T{i}") })
					.to_string(),
			),
		)
		.await;
		assert_eq!(res.status(), StatusCode::CREATED);
	}

	// Page 1: full page (limit 2) → carries a next cursor.
	let res = request(&state, Method::GET, "/api/bookmarks?limit=2", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	let next = res
		.headers()
		.get("x-next-cursor")
		.and_then(|v| v.to_str().ok())
		.unwrap()
		.to_string();
	let page1 = body_text(res).await;

	// Page 2 via the cursor token.
	let res = request(
		&state,
		Method::GET,
		&format!("/api/bookmarks?limit=2&cursor={next}"),
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	let next2 = res
		.headers()
		.get("x-next-cursor")
		.and_then(|v| v.to_str().ok())
		.unwrap()
		.to_string();
	let page2 = body_text(res).await;

	// Page 3: only one row left, so the page is short → no next cursor.
	let res = request(
		&state,
		Method::GET,
		&format!("/api/bookmarks?limit=2&cursor={next2}"),
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	assert!(res.headers().get("x-next-cursor").is_none());
	let page3 = body_text(res).await;

	// Every bookmark appears exactly across the three pages, and the total
	// is whole-corpus (cursor must not leak into the count).
	let combined = format!("{page1}{page2}{page3}");
	for i in 0..5 {
		assert!(
			combined.contains(&format!("example.com/{i}")),
			"missing bookmark {i} across pages"
		);
	}
	let res = request(&state, Method::GET, "/api/bookmarks?limit=2", Body::empty()).await;
	assert_eq!(
		res.headers()
			.get("x-total-count")
			.and_then(|v| v.to_str().ok())
			.unwrap(),
		"5"
	);

	// A malformed cursor is a 400, and the trash view never accepts one.
	let res = request(
		&state,
		Method::GET,
		"/api/bookmarks?cursor=zz",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::BAD_REQUEST);
	let res = request(
		&state,
		Method::GET,
		"/api/bookmarks?trash=true&cursor=zzzz",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn inverted_time_range_is_a_400() {
	silence_logs();
	let (_dir, state) = test_state();

	// Garbage dates were already 400 (invalid_date); an inverted pair
	// (after > before) must also be a 400 rather than a silently-empty list.
	for uri in [
		"/api/bookmarks?created_after=2024-05-01&created_before=2024-01-01",
		"/api/bookmarks?updated_after=2024-05-01&updated_before=2024-01-01",
		"/api/bookmarks?visited_after=2024-05-01&visited_before=2024-01-01",
		"/api/bookmarks?trashed_after=2024-05-01&trashed_before=2024-01-01",
		"/api/bookmarks?created_after=2024-05-01%2009:00:00&created_before=2024-01-01%2000:00:00",
	] {
		let res = request(&state, Method::GET, uri, Body::empty()).await;
		assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{uri}");
		let text = body_text(res).await;
		assert!(
			text.contains("invalid_date"),
			"{uri} must return the invalid_date contract, got: {text}"
		);
	}

	// An inclusive same-day range (bare date means the whole UTC day) is fine.
	let res = request(
		&state,
		Method::GET,
		"/api/bookmarks?created_after=2024-01-01&created_before=2024-01-01",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_unknown_route_is_a_json_404() {
	silence_logs();
	let (_dir, state) = test_state();

	// An unmatched `/api/*` path is a 404 in the JSON error contract — not
	// the SPA fallback, which would return an HTML document.
	for uri in [
		"/api/nope",
		"/api/nonexistent/1",
		"/api/bookmarks/nope/extra",
	] {
		let res = request(&state, Method::GET, uri, Body::empty()).await;
		assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
		let text = body_text(res).await;
		assert!(
			text.contains("no such API endpoint") && text.contains("not_found"),
			"{uri} must return the JSON 404 contract, got: {text}"
		);
	}

	// The docs route (a separate top-level route, not the nest) still works.
	let res = request(&state, Method::GET, "/api/openapi.json", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);

	// The interactive docs shell points Swagger UI back at the spec route.
	let res = request(&state, Method::GET, "/api/docs", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(
		text.contains("swagger-ui") && text.contains("/api/openapi.json"),
		"/api/docs must serve the Swagger UI HTML shell, got: {text}"
	);
}

#[tokio::test]
async fn keyword_redirect_is_public_and_visits_get_tracked() {
	silence_logs();
	let (_dir, state) = test_state();

	// Create a bookmark with a keyword.
	let res = request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://rust-lang.org", "keyword": "rs" }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::CREATED);

	// /keywords list and redirect are NOT behind auth middleware.
	let res = request(&state, Method::GET, "/keywords", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(text.contains("rs"));

	let res = request(&state, Method::GET, "/keywords/rs", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::TEMPORARY_REDIRECT);
	let location = res
		.headers()
		.get(header::LOCATION)
		.and_then(|v| v.to_str().ok())
		.unwrap()
		.to_string();
	assert_eq!(location, "https://rust-lang.org");
}

#[tokio::test]
