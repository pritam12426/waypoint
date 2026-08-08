use std::net::SocketAddr;
use std::sync::{Arc, Once};

use axum::{
	body::Body,
	extract::ConnectInfo,
	http::{Method, Request, StatusCode, header},
};
use serde_json::json;
use tower::ServiceExt;

use waypoint::database;
use waypoint::http::{AppState, app};

static SILENCE: Once = Once::new();

fn silence_logs() {
	SILENCE.call_once(|| {
		waypoint::logging::log_init(
			None,
			waypoint::logging::LogLevel::Off,
			waypoint::logging::LogFormat::Pretty,
		);
	});
}

fn test_state() -> (tempfile::TempDir, AppState) {
	let dir = tempfile::tempdir().unwrap();
	let db = database::Db::open(dir.path().join("waypoint.db")).unwrap();
	let state = AppState {
		db: Arc::new(db),
		counts: Arc::new(waypoint::http::CountCache::new()),
		stats: Arc::new(waypoint::http::StatsCache::new()),
		static_dir: None,
		api_token: None,
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

	// Distinct aggregate keys are cached separately; a write drops the cache.
	let res = request(
		&state,
		Method::POST,
		"/api/bookmarks",
		Body::from(json!({ "url": "https://d.example" }).to_string()),
	)
	.await;
	assert_eq!(res.status(), StatusCode::CREATED);
	let res = request(&state, Method::GET, "/api/stats", Body::empty()).await;
	assert_eq!(res.status(), StatusCode::OK);
	assert_ne!(body_text(res).await, first_body);
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
