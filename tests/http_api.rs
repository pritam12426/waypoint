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

