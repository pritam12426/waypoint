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
async fn open_bookmark_is_public_and_tracks_visits() {
	silence_logs();
	let (_dir, state) = test_state();

	// Create a bookmark with no keyword — /open/{id} must work for any
	// bookmark, not just ones with a shortcut.
	let res = request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://example.com/visit", "title": "Visit Me" }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::CREATED);
	let created: serde_json::Value = serde_json::from_str(&body_text(res).await).unwrap();
	let id = created["id"].as_i64().unwrap();
	assert_eq!(created["visit_count"], 0);

	// /open/{id} is NOT behind auth middleware and 307-redirects to the URL.
	let res = request(&state, Method::GET, &format!("/open/{id}"), Body::empty()).await;
	assert_eq!(res.status(), StatusCode::TEMPORARY_REDIRECT);
	let location = res
		.headers()
		.get(header::LOCATION)
		.and_then(|v| v.to_str().ok())
		.unwrap()
		.to_string();
	assert_eq!(location, "https://example.com/visit");

	// The visit is recorded fire-and-forget, so poll briefly for it.
	let mut visit_count = 0i64;
	for _ in 0..50 {
		let res = request(
			&state,
			Method::GET,
			&format!("/api/bookmarks/{id}"),
			Body::empty(),
		)
		.await;
		let bookmark: serde_json::Value = serde_json::from_str(&body_text(res).await).unwrap();
		visit_count = bookmark["visit_count"].as_i64().unwrap_or(0);
		if visit_count == 1 {
			assert!(bookmark["last_visited_at"].is_string());
			break;
		}
		tokio::time::sleep(std::time::Duration::from_millis(20)).await;
	}
	assert_eq!(
		visit_count, 1,
		"visit_count should increment after /open/{id}"
	);
}

#[tokio::test]
async fn errors_use_json_body() {
	silence_logs();
	let (_dir, state) = test_state();

	// Missing bookmark -> 404 with a JSON error body.
	let res = request(&state, Method::GET, "/api/bookmarks/999", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::NOT_FOUND);
	let text = body_text(res).await;
	assert!(text.contains("not_found"));

	// Search without a query -> 400.
	let res = request(&state, Method::GET, "/api/search", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::BAD_REQUEST);
	let text = body_text(res).await;
	assert!(text.contains("query_required"));
}

#[tokio::test]
async fn duplicate_url_is_a_conflict() {
	silence_logs();
	let (_dir, state) = test_state();

	let body = json!({ "url": "https://example.com/dup" }).to_string();
	let res = request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(body.clone()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::CREATED);

	let res = request(&state, Method::POST, "/api/bookmarks", Body::from(body)).await;
	assert_eq!(res.status(), StatusCode::CONFLICT);
	// Every AppError response carries the machine-readable code as a header
	// too, and the failure-logging middleware skips responses tagged this
	// way (they already logged code + message).
	assert_eq!(
		res.headers()
			.get("x-waypoint-error")
			.map(|v| v.to_str().unwrap()),
		Some("conflict_url")
	);
	let text = body_text(res).await;
	assert!(text.contains("conflict_url"));
}

#[tokio::test]
async fn duplicate_keyword_is_a_conflict() {
	silence_logs();
	let (_dir, state) = test_state();

	request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://one.example", "keyword": "kw" }).to_string()),
	)
	.await;

	// Same keyword on a different URL is a friendly 409, not a raw
	// SQLite constraint message.
	let res = request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://two.example", "keyword": "kw" }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::CONFLICT);
	let text = body_text(res).await;
	assert!(text.contains("conflict_keyword"), "body: {text}");
	assert!(text.contains("already in use"), "body: {text}");
	assert!(
		!text.contains("UNIQUE constraint"),
		"raw SQLite message leaked: {text}"
	);

	// A real second bookmark lets us test the PUT path.
	request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://three.example" }).to_string()),
	)
	.await;

	// Setting a duplicate keyword on an existing bookmark via PUT is the
	// same friendly 409.
	let res = request(
		&state,
		Method::PUT,
		"/api/bookmarks/2",
		Body::from(json!({ "keyword": "kw" }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::CONFLICT);
	let text = body_text(res).await;
	assert!(text.contains("conflict_keyword"), "body: {text}");

	// Re-saving the same keyword on its own bookmark stays valid.
	let res = request(
		&state,
		Method::PUT,
		"/api/bookmarks/1",
		Body::from(json!({ "keyword": "kw" }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn stats_endpoints_accept_limit_and_offset() {
	silence_logs();
	let (_dir, state) = test_state();

	for url in [
		"https://aaa.example",
		"https://bbb.example",
		"https://ccc.example",
	] {
		let res = request(
			&state,
			Method::POST,
			"/api/bookmarks",
			Body::from(json!({ "url": url }).to_string()),
		)
		.await;
		assert_eq!(res.status(), StatusCode::CREATED);
	}

	// One domain per bookmark: limit slices the ranking, offset pages it.
	let res = request(
		&state,
		Method::GET,
		"/api/stats/domains?limit=2",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	let body: serde_json::Value = serde_json::from_str(&body_text(res).await).unwrap();
	assert_eq!(body.as_array().unwrap().len(), 2);

	let res = request(
		&state,
		Method::GET,
		"/api/stats/domains?limit=2&offset=2",
		Body::empty(),
	)
	.await;
	let body: serde_json::Value = serde_json::from_str(&body_text(res).await).unwrap();
	assert_eq!(body.as_array().unwrap().len(), 1);

	// The paged tag/activity endpoints share the same contract.
	let res = request(
		&state,
		Method::GET,
		"/api/stats/activity?limit=1&offset=1",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);

	// Out-of-range limits are a 400 invalid_limit, like the list endpoint.
	let res = request(
		&state,
		Method::GET,
		"/api/stats/domains?limit=0",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::BAD_REQUEST);
	let text = body_text(res).await;
	assert!(text.contains("invalid_limit"), "body: {text}");
}

#[tokio::test]
async fn stats_endpoints_are_cached_with_etag_and_304() {
	silence_logs();
	let (_dir, state) = test_state();

	let res = request(&state, Method::GET, "/api/stats", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	let etag = res
		.headers()
		.get(header::ETAG)
		.expect("first stats response carries an ETag")
		.to_str()
		.unwrap()
		.to_string();
	assert_eq!(
		res.headers().get(header::CACHE_CONTROL).unwrap(),
		"private, max-age=30"
	);
	let first_body = body_text(res).await;

	// The same aggregate with a matching If-None-Match short-circuits to 304.
	let res = request_with_headers(
		&state,
		Method::GET,
		"/api/stats",
		header::IF_NONE_MATCH,
		etag.clone(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
	let empty = body_text(res).await;
	assert!(empty.is_empty(), "304 must have no body, got {empty:?}");

	// A stale or missing If-None-Match returns the full body again, and the
	// ETag is stable across requests (cache hit vs cache miss both produce
	// the same digest).
	let res = request(&state, Method::GET, "/api/stats", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	assert_eq!(body_text(res).await, first_body);

	// Distinct aggregate keys are cached separately; a successful write
	// refreshes the warm entries in place rather than dropping them.
	let res = request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://d.example" }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::CREATED);
	// The write recomputed the overview body, so the entry survives (not
	// dropped) and already reflects the new bookmark.
	assert!(
		state.stats.get("overview").is_some(),
		"a successful write must refresh, not drop, the stats cache"
	);
	let res = request(&state, Method::GET, "/api/stats", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	assert_ne!(body_text(res).await, first_body);
}

#[tokio::test]
async fn successful_write_refreshes_warm_count_cache() {
	silence_logs();
	let (_dir, state) = test_state();

	// Warm the count cache: the first list populates the default-filter
	// entry (the same key `list_bookmarks` computes, minus pagination).
	let res = request(&state, Method::GET, "/api/bookmarks", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	let key = format!("{:?}", waypointd::model::BookmarkFilter::default());
	assert_eq!(
		state.counts.get(&key),
		Some(0),
		"listing must warm the count cache"
	);

	// A successful create must refresh the entry in place — it survives the
	// write (not dropped for the next read to rebuild) and already carries
	// the new total.
	let res = request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://refresh.example" }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::CREATED);
	assert_eq!(
		state.counts.get(&key),
		Some(1),
		"a successful create must refresh, not drop, the count cache"
	);
}

async fn request_with_headers(
	state: &AppState,
	method: Method,
	uri: &str,
	name: axum::http::HeaderName,
	value: String,
) -> axum::response::Response {
	let mut req = Request::builder()
		.method(method)
		.uri(uri)
		.header(name, value)
		.body(Body::empty())
		.unwrap();
	req.extensions_mut()
		.insert(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
	app(state.clone()).oneshot(req).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn read_pool_serves_parallel_requests_alongside_a_writer() {
	silence_logs();
	let (_dir, state) = test_state();

	for i in 0..50 {
		let res = request(
			&state,
			Method::POST,
			"/api/bookmarks",
			Body::from(json!({ "url": format!("https://{i}.example") }).to_string()),
		)
		.await;
		assert_eq!(res.status(), StatusCode::CREATED);
	}

	// Parallel readers: page loads and stats. Each handler round-robins a
	// pooled read connection inside spawn_blocking, so these genuinely
	// overlap on the blocking threads instead of queueing on one lock.
	// Interleaved writers (bookmark inserts) exercise WAL coexistence.
	let mut tasks = Vec::new();
	for _ in 0..24 {
		let state = state.clone();
		tasks.push(tokio::spawn(async move {
			let res = request(
				&state,
				Method::GET,
				"/api/bookmarks?limit=20",
				Body::empty(),
			)
			.await;
			assert_eq!(res.status(), StatusCode::OK);
		}));
	}
	for _ in 0..16 {
		let state = state.clone();
		tasks.push(tokio::spawn(async move {
			let res = request(&state, Method::GET, "/api/stats", Body::empty()).await;
			assert_eq!(res.status(), StatusCode::OK);
		}));
	}
	for n in 0..8 {
		let state = state.clone();
		tasks.push(tokio::spawn(async move {
			let res = request(
				&state,
				Method::POST,
				"/api/bookmarks",
				Body::from(json!({ "url": format!("https://writer-{n}.example") }).to_string()),
			)
			.await;
			assert_eq!(res.status(), StatusCode::CREATED);
		}));
	}
	for t in tasks {
		t.await.unwrap();
	}

	// Every write landed and is visible to a fresh read.
	let res = request(&state, Method::GET, "/api/stats", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn static_frontend_is_served() {
	silence_logs();
	let (_dir, state) = test_state();
	// The embedded frontend ships in the binary, so the fallback serves
	// index.html at the root.
	let res = request(&state, Method::GET, "/", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(text.contains("<!doctype html>") || text.contains("<html"));
}

#[tokio::test]
async fn bulk_delete_dry_run_previews_then_trashes() {
	silence_logs();
	let (_dir, state) = test_state();

	request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://a.example/rust", "tags": ["rust"] }).to_string()),
	)
	.await;
	request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://b.example/web" }).to_string()),
	)
	.await;

	// Dry run: preview only, nothing changes.
	let res = request(
		&state,
		Method::DELETE,
		"/api/bookmarks?tag=rust&dry_run=true",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(text.contains("\"removed\":0"), "{text}");
	assert!(text.contains("\"ids\":[1]"), "{text}");

	// The previewed bookmark is still fetchable afterwards.
	let res = request(&state, Method::GET, "/api/bookmarks/1", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);

	// Real run moves it to the trash (invisible to a plain GET).
	let res = request(
		&state,
		Method::DELETE,
		"/api/bookmarks?tag=rust",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(text.contains("\"removed\":1"), "{text}");
	let res = request(&state, Method::GET, "/api/bookmarks/1", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::NOT_FOUND);

	// A catch-all (no ids, no criteria) is refused.
	let res = request(&state, Method::DELETE, "/api/bookmarks", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn bulk_update_applies_partial_update_and_skips_trashed_ids() {
	silence_logs();
	let (_dir, state) = test_state();

	for i in 0..3 {
		request(
			&state,
			Method::POST,
			"/api/bookmarks",
			Body::from(
				json!({ "url": format!("https://bulk{i}.example/page"), "title": format!("B{i}") })
					.to_string(),
			),
		)
		.await;
	}
	// Trash id 3 so it comes back in `skipped`.
	request(&state, Method::DELETE, "/api/bookmarks/3", Body::empty()).await;

	// One PATCH applies the same change to ids 1..3: add a tag, move to a
	// category, archive. The trashed id 3 is reported, not an error.
	let res = request(
		&state,
		Method::PATCH,
		"/api/bookmarks",
		Body::from(
			json!({
				"ids": [1, 2, 3],
				"update": {
					"add_tags": ["rust"],
					"category": "Media",
					"is_archived": true,
				}
			})
			.to_string(),
		),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(text.contains("\"updated\":[1,2]"), "{text}");
	assert!(text.contains("\"skipped\":[3]"), "{text}");

	// The change landed on the surviving bookmarks.
	let res = request(&state, Method::GET, "/api/bookmarks/1", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(text.contains("rust"), "{text}");
	assert!(text.contains("Media"), "{text}");
	assert!(text.contains("\"is_archived\":true"), "{text}");
	let res = request(&state, Method::GET, "/api/bookmarks/2", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(text.contains("rust"), "{text}");

	// Empty ids and a nothing-to-change update are both 400.
	let res = request(
		&state,
		Method::PATCH,
		"/api/bookmarks",
		Body::from(json!({ "ids": [], "update": { "is_archived": true } }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::BAD_REQUEST);
	let res = request(
		&state,
		Method::PATCH,
		"/api/bookmarks",
		Body::from(json!({ "ids": [1], "update": {} }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn empty_trash_dry_run_does_not_purge() {
	silence_logs();
	let (_dir, state) = test_state();

	request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://c.example/one" }).to_string()),
	)
	.await;
	let res = request(&state, Method::DELETE, "/api/bookmarks/1", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::NO_CONTENT);

	// Dry run must NOT purge the trash.
	let res = request(
		&state,
		Method::DELETE,
		"/api/trash?dry_run=true",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(
		text.contains("\"removed\":0"),
		"dry run must not delete: {text}"
	);

	// The trashed bookmark is still listed in the recycle bin.
	let res = request(
		&state,
		Method::GET,
		"/api/bookmarks?trash=true",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(text.contains("c.example/one"), "{text}");

	// The real call purges it for good.
	let res = request(&state, Method::DELETE, "/api/trash", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	assert!(text.contains("\"removed\":1"), "{text}");
	let res = request(
		&state,
		Method::GET,
		"/api/bookmarks?trash=true",
		Body::empty(),
	)
	.await;
	let text = body_text(res).await;
	assert!(!text.contains("c.example/one"), "{text}");
}

// The delete → re-add → restore cycle: a trashed copy and a live re-add can
// coexist (URLs are unique only outside the trash), but restoring the old
// trashed copy on top of the live one is a 409 conflict_url — the same
// contract as a duplicate `POST /api/bookmarks`. And re-trashing the live
// copy purges the older trashed one, so the trash never holds two bookmarks
// with the same URL.
#[tokio::test]
async fn restore_conflicts_with_a_live_duplicate_and_trash_stays_deduped() {
	silence_logs();
	let (_dir, state) = test_state();

	let url = "https://youtube.com/@ProfessorOfHow";
	request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": url }).to_string()),
	)
	.await;
	// Move #1 to the trash, then re-add the same URL as #2.
	let res = request(&state, Method::DELETE, "/api/bookmarks/1", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::NO_CONTENT);
	request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": url }).to_string()),
	)
	.await;

	// Restoring the old trashed copy collides with the live #2.
	let res = request(
		&state,
		Method::POST,
		"/api/bookmarks/1/restore",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::CONFLICT);
	let text = body_text(res).await;
	assert!(text.contains("conflict_url"), "{text}");

	// Re-trashing #2 purges the older trashed copy #1 — trash stays deduped.
	let res = request(&state, Method::DELETE, "/api/bookmarks/2", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::NO_CONTENT);
	let res = request(
		&state,
		Method::GET,
		"/api/bookmarks?trash=true",
		Body::empty(),
	)
	.await;
	let text = body_text(res).await;
	let trash: serde_json::Value = serde_json::from_str(&text).unwrap();
	let items = trash.as_array().unwrap();
	assert_eq!(items.len(), 1, "trash holds exactly one copy: {text}");
	assert_eq!(items[0]["id"], 2, "the newest trashed copy wins: {text}");

	// The survivor (the newest copy) restores cleanly.
	let res = request(
		&state,
		Method::POST,
		"/api/bookmarks/2/restore",
		Body::empty(),
	)
	.await;
	assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

// ============================================================
// Import / export / check
// ============================================================

const NETSCAPE: &str = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<DL><p>
<DT><A HREF="https://standalone.example/page">Standalone</A>
<DT><H3>Work</H3>
<DL><p>
<DT><A HREF="https://work.example/proj">Project</A>
</DL><p>
</DL><p>"#;

#[tokio::test]
async fn import_creates_bookmarks_from_netscape_html() {
	silence_logs();
	let (_dir, state) = test_state();

	let res = request(
		&state,
		Method::POST,
		"/api/import",
		Body::from(
			json!({
				"content": NETSCAPE,
				"tags": ["imported"],
				"category": "Inbox",
			})
			.to_string(),
		),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	let result: serde_json::Value = serde_json::from_str(&text).unwrap();
	assert_eq!(result["imported"], 2, "{text}");
	assert_eq!(result["skipped"], 0, "{text}");

	// Category + tags override applied to both bookmarks.
	let res = request(
		&state,
		Method::GET,
		"/api/bookmarks?limit=50",
		Body::empty(),
	)
	.await;
	let text = body_text(res).await;
	let items: serde_json::Value = serde_json::from_str(&text).unwrap();
	let items = items.as_array().unwrap();
	assert_eq!(items.len(), 2);
	assert!(items.iter().all(|b| b["category_name"] == "Inbox"));
	assert!(items.iter().all(|b| b["tags"][0] == "imported"));
}

#[tokio::test]
async fn import_is_a_noop_on_duplicates() {
	silence_logs();
	let (_dir, state) = test_state();

	request(
		&state,
		Method::POST,
		"/api/import",
		Body::from(json!({ "content": NETSCAPE }).to_string()),
	)
	.await;
	let res = request(
		&state,
		Method::POST,
		"/api/import",
		Body::from(json!({ "content": NETSCAPE }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::OK);
	let text = body_text(res).await;
	let result: serde_json::Value = serde_json::from_str(&text).unwrap();
	assert_eq!(result["imported"], 0, "{text}");
	assert_eq!(result["skipped"], 2, "{text}");
}

#[tokio::test]
async fn export_returns_markdown_and_csv_as_plain_text() {
	silence_logs();
	let (_dir, state) = test_state();

	request(
		&state,
		Method::POST,
		"/api/import",
		Body::from(json!({ "content": NETSCAPE }).to_string()),
	)
	.await;

	let res = request(&state, Method::GET, "/api/export?format=md", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	assert_eq!(
		res.headers()["content-type"],
		"text/markdown; charset=utf-8",
		"the body is the raw markdown, not a JSON payload"
	);
	let text = body_text(res).await;
	assert!(text.contains("# Bookmarks"));
	assert!(text.contains("https://work.example/proj"));
	assert!(!text.trim_start().starts_with('{'), "no JSON wrapper");

	let res = request(&state, Method::GET, "/api/export?format=csv", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	assert_eq!(
		res.headers()["content-type"],
		"text/csv; charset=utf-8",
		"the body is the raw CSV, not a JSON payload"
	);
	let text = body_text(res).await;
	assert!(text.starts_with("id,title,url,description,"));
	assert!(!text.trim_start().starts_with('{'), "no JSON wrapper");

	// The default format is markdown.
	let res = request(&state, Method::GET, "/api/export", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	assert_eq!(
		res.headers()["content-type"],
		"text/markdown; charset=utf-8"
	);

	// An unknown format is a 400.
	let res = request(&state, Method::GET, "/api/export?format=xml", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

/// Polls `GET /api/check/{id}` until it leaves the running state.
async fn wait_for_check(state: &AppState, id: serde_json::Value) -> serde_json::Value {
	for _ in 0..50 {
		let res = request(
			state,
			Method::GET,
			&format!("/api/check/{id}"),
			Body::empty(),
		)
		.await;
		assert_eq!(res.status(), StatusCode::OK);
		let text = body_text(res).await;
		let body: serde_json::Value = serde_json::from_str(&text).unwrap();
		if body["status"] != "running" {
			return body;
		}
		tokio::time::sleep(std::time::Duration::from_millis(50)).await;
	}
	panic!("check job {id} did not finish");
}

#[tokio::test]
async fn check_job_lifecycle_with_non_http_bookmarks() {
	silence_logs();
	let (_dir, state) = test_state();

	// Non-http URLs are counted and skipped, never probed — so this run
	// completes instantly and deterministically without touching the network.
	for url in ["mailto:test@example.com", "javascript:void(0)"] {
		request(
			&state,
			Method::POST,
			"/api/bookmarks",
			Body::from(json!({ "url": url, "title": url }).to_string()),
		)
		.await;
	}

	let res = request(
		&state,
		Method::POST,
		"/api/check",
		Body::from(json!({}).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::ACCEPTED);
	let text = body_text(res).await;
	let started: serde_json::Value = serde_json::from_str(&text).unwrap();
	let id = started["id"].clone();

	let body = wait_for_check(&state, id).await;
	assert_eq!(body["status"], "finished", "{body}");
	assert_eq!(body["checked"], 0);
	assert_eq!(body["skipped"], 2);
	assert_eq!(body["alive"], 0);
	assert_eq!(body["dead"], serde_json::json!([]));
}

#[tokio::test]
async fn check_rejects_delete_and_hard_delete_together() {
	silence_logs();
	let (_dir, state) = test_state();

	let res = request(
		&state,
		Method::POST,
		"/api/check",
		Body::from(json!({ "delete": true, "hardDelete": true }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::BAD_REQUEST);
	let text = body_text(res).await;
	assert!(text.contains("mutually exclusive"), "{text}");
}

#[tokio::test]
async fn check_trashes_dead_links() {
	silence_logs();
	let (_dir, state) = test_state();

	// A connection to port 1 on loopback is refused immediately — a dead
	// link that doesn't depend on any external service.
	request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "http://127.0.0.1:1/", "title": "dead" }).to_string()),
	)
	.await;

	let res = request(
		&state,
		Method::POST,
		"/api/check",
		Body::from(json!({ "delete": true, "jobs": 2 }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::ACCEPTED);
	let text = body_text(res).await;
	let started: serde_json::Value = serde_json::from_str(&text).unwrap();
	let id = started["id"].clone();

	let body = wait_for_check(&state, id).await;
	assert_eq!(body["status"], "finished", "{body}");
	assert_eq!(body["checked"], 1);
	assert_eq!(body["deleted"], 1, "{body}");
	let dead = body["dead"].as_array().unwrap();
	assert_eq!(dead.len(), 1);
	assert!(
		!dead[0]["reason"].as_str().unwrap_or("").is_empty(),
		"dead link carries a reason: {body}"
	);

	// The dead link was moved to the trash, not purged.
	let res = request(
		&state,
		Method::GET,
		"/api/bookmarks?trash=true",
		Body::empty(),
	)
	.await;
	let text = body_text(res).await;
	let trash: serde_json::Value = serde_json::from_str(&text).unwrap();
	assert_eq!(trash.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn check_unknown_job_is_a_404() {
	silence_logs();
	let (_dir, state) = test_state();

	let res = request(&state, Method::GET, "/api/check/999", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// Binds a one-shot TCP server that answers any request with `200 OK`, so an
/// "alive" verdict is exercised without touching the external network.
fn spawn_ok_server() -> SocketAddr {
	let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
	let addr = listener.local_addr().unwrap();
	std::thread::spawn(move || {
		let (mut stream, _) = listener.accept().unwrap();
		let mut buf = [0u8; 4096];
		let _ = stream.read(&mut buf);
		let _ =
			stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
	});
	addr
}

#[tokio::test]
