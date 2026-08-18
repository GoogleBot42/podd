//! Static-SPA serving tests: caching headers and history-fallback behavior.
//!
//! Regression tests for the 2026-08-17 white-page incident: the UI ships from
//! a Nix build with epoch mtimes, so `Last-Modified` revalidation answered 304
//! forever and stale browsers could never pick up a redeployed index.html —
//! while the old history-fallback served index.html (text/html, 200) in place
//! of missing hashed assets, failing the module-script MIME check.

use api::{router, MockControl, PodControl, StateStore};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

fn spa_app() -> axum::Router {
    let store = Arc::new(StateStore::in_memory());
    let control = Arc::new(MockControl::new());
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spa");
    router(store, control as Arc<dyn PodControl>, Some(dir))
}

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

fn get_conditional(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("if-modified-since", "Thu, 01 Jan 1970 00:00:01 GMT")
        .body(Body::empty())
        .unwrap()
}

fn header<'a>(resp: &'a axum::response::Response, name: &str) -> &'a str {
    resp.headers()
        .get(name)
        .map(|v| v.to_str().unwrap())
        .unwrap_or("")
}

#[tokio::test]
async fn index_served_fresh_with_no_store() {
    let resp = spa_app().oneshot(get("/")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(header(&resp, "content-type").starts_with("text/html"));
    assert_eq!(header(&resp, "cache-control"), "no-store");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&body)
        .unwrap()
        .contains("podd spa fixture"));
}

#[tokio::test]
async fn index_ignores_conditional_requests() {
    // A stale client revalidating its cached index must get a full 200, not
    // a 304 — the epoch mtimes mean 304 would pin the stale copy forever.
    let resp = spa_app().oneshot(get_conditional("/")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(header(&resp, "cache-control"), "no-store");
}

#[tokio::test]
async fn history_fallback_serves_index_for_spa_routes() {
    let resp = spa_app().oneshot(get("/settings")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(header(&resp, "content-type").starts_with("text/html"));
    assert_eq!(header(&resp, "cache-control"), "no-store");
}

#[tokio::test]
async fn hashed_assets_are_immutable() {
    let resp = spa_app().oneshot(get("/assets/app-test.js")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        header(&resp, "cache-control"),
        "public, max-age=31536000, immutable"
    );
}

#[tokio::test]
async fn missing_asset_is_404_not_index_html() {
    let resp = spa_app()
        .oneshot(get("/assets/index-OLDHASH.js"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(!header(&resp, "content-type").starts_with("text/html"));
}

#[tokio::test]
async fn non_asset_static_files_get_no_store() {
    // Icons/manifest also carry epoch mtimes; only revalidation-proof
    // policies are safe.
    let resp = spa_app().oneshot(get("/index.html")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(header(&resp, "cache-control"), "no-store");
}

#[tokio::test]
async fn api_responses_not_touched_by_cache_policy() {
    let resp = spa_app().oneshot(get("/api/deviceStatus")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(header(&resp, "cache-control"), "");
}
