//! free-sleep-compatible HTTP API + static-SPA server for the `podd` daemon.
//!
//! This crate is deliberately decoupled from hardware: all device commands go
//! through the [`PodControl`] trait, and all state lives in a [`StateStore`].
//! Wire the trait to `podd-core` and feed the store from a live state bus in a
//! follow-up; nothing here touches a UART or `podd-core`.
//!
//! ```no_run
//! use std::sync::Arc;
//! # async fn run() -> anyhow::Result<()> {
//! let store = Arc::new(api::StateStore::in_memory());
//! let control = Arc::new(api::MockControl::new());
//! let addr = "127.0.0.1:3000".parse().unwrap();
//! api::serve(addr, store, control, None).await
//! # }
//! ```

pub mod control;
pub mod error;
pub mod handlers;
pub mod metrics;
pub mod state;
pub mod updates;
pub mod wire;

pub use control::{Call, MockControl, NotImplemented, PoddControl, PodControl};
pub use state::{StateStore, StoreConfig};
pub use updates::{DaemonBuild, MockUpdates, UpdateOps, UpdateStatus, UpdatesReport};

use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::Router;
use handlers::AppState;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::services::ServeDir;

/// True for localhost / RFC1918 LAN origins (10/8, 172.16/12, 192.168/16).
fn origin_allowed(origin: &HeaderValue) -> bool {
    let Ok(s) = origin.to_str() else {
        return false;
    };
    // Strip scheme, then take the host portion (before any ':' port).
    let hostport = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))
        .unwrap_or(s);
    let hostport = hostport.split('/').next().unwrap_or(hostport);
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        // Bracketed IPv6 (`[::1]:3000`) — the port split below would cut at
        // the wrong colon.
        rest.split(']').next().unwrap_or(rest)
    } else {
        hostport.rsplit_once(':').map(|(h, _)| h).unwrap_or(hostport)
    };

    if host == "localhost" {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => false,
    }
}

fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _parts| origin_allowed(origin)))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(Any)
}

/// Build the full API router: `/api/*` JSON endpoints plus (optionally) a static
/// SPA served at all other paths with history-fallback to `index.html`.
///
/// When `spa_dir` is `None` the server is API-only (non-`/api` routes 404).
pub fn router(
    store: Arc<StateStore>,
    control: Arc<dyn PodControl>,
    spa_dir: Option<PathBuf>,
) -> Router {
    router_with_vitals(store, control, spa_dir, None)
}

/// [`router`] plus a vitals history store backing `/metrics/vitals*`.
pub fn router_with_vitals(
    store: Arc<StateStore>,
    control: Arc<dyn PodControl>,
    spa_dir: Option<PathBuf>,
    vitals: Option<Arc<podd_core::biometrics::VitalsStore>>,
) -> Router {
    router_full(store, control, spa_dir, vitals, None)
}

/// [`router_with_vitals`] plus the update agent backing `/updates*`
/// (`REPLACEMENT_PLAN` §9). `updates: None` leaves those routes reporting
/// "no update agent is running" rather than a fabricated status.
pub fn router_full(
    store: Arc<StateStore>,
    control: Arc<dyn PodControl>,
    spa_dir: Option<PathBuf>,
    vitals: Option<Arc<podd_core::biometrics::VitalsStore>>,
    updates: Option<Arc<dyn UpdateOps>>,
) -> Router {
    let app_state = AppState {
        store,
        control,
        vitals,
        updates,
    };

    let api = Router::new()
        .route(
            "/deviceStatus",
            get(handlers::get_device_status).post(handlers::post_device_status),
        )
        .route(
            "/settings",
            get(handlers::get_settings).post(handlers::post_settings),
        )
        .route(
            "/schedules",
            get(handlers::get_schedules).post(handlers::post_schedules),
        )
        // podd-only (free-sleep has no MQTT): the broker link's settings (#18)
        .route("/mqtt", get(handlers::get_mqtt).post(handlers::post_mqtt))
        .route("/alarm", post(handlers::post_alarm))
        .route("/execute", post(handlers::post_execute))
        .route("/jobs", post(handlers::post_jobs))
        .route(
            "/services",
            get(handlers::get_services).post(handlers::post_services),
        )
        .route("/serverStatus", get(handlers::get_server_status))
        // update observability + the two controls pod-updater implements
        // (REPLACEMENT_PLAN §9; issue #1). Applying an update is deliberately
        // not routed here — see crates/api/src/updates.rs.
        .route("/updates", get(handlers::get_updates))
        .route("/updates/check", post(handlers::post_updates_check))
        .route("/updates/rollback", post(handlers::post_updates_rollback))
        .route("/logs", get(handlers::get_logs))
        .route("/logs/{filename}", get(handlers::get_log_stream))
        .route(
            "/metrics/presence",
            get(handlers::get_presence).post(handlers::post_presence),
        )
        // biometrics: vitals are real (#12, backed by the store passed to
        // router_with_vitals); sleep/movement remain UI-friendly empties that
        // still honour ?startTime/?endTime/?side (#108)
        .route("/metrics/sleep", get(handlers::get_sleep_records))
        .route(
            "/metrics/sleep/{id}",
            put(handlers::sleep_put).delete(handlers::sleep_delete),
        )
        .route("/metrics/vitals", get(handlers::get_vitals_records))
        .route("/metrics/vitals/summary", get(handlers::vitals_summary))
        .route("/metrics/movement", get(handlers::get_movement_records))
        .fallback(|| async { error::not_found() })
        .with_state(app_state);

    let mut app = Router::new().nest("/api", api);

    app = match spa_dir {
        Some(dir) => {
            let index = dir.join("index.html");
            let serve_dir = ServeDir::new(&dir)
                .append_index_html_on_directories(false)
                .fallback(get(move |req: Request| spa_fallback(index.clone(), req)));
            app.fallback_service(serve_dir)
                .layer(middleware::from_fn(static_cache_policy))
        }
        None => app.fallback(|| async { error::not_found() }),
    };

    app.layer(cors_layer())
}

/// History-fallback for the SPA: any non-file path gets a *fresh* `index.html`.
///
/// The UI ships from a Nix build where every file mtime is the Unix epoch, so
/// a `ServeFile` fallback here answers `If-Modified-Since` with 304 forever —
/// after a deploy changes the hashed asset names, browsers that cached the old
/// index.html can never recover (white page; hit live 2026-08-17). Reading the
/// file per-request and ignoring conditional headers means one ordinary reload
/// un-sticks a stale client.
async fn spa_fallback(index: PathBuf, req: Request) -> Response {
    if req.uri().path().starts_with("/assets/") {
        // A missing hashed asset means the client is holding a stale
        // index.html. The old history-fallback served index.html *as* the
        // asset, which fails the browser's module-script MIME check and
        // white-screens the app — a 404 at least fails loudly.
        return error::not_found().into_response();
    }
    match tokio::fs::read(&index).await {
        Ok(body) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            body,
        )
            .into_response(),
        Err(err) => {
            log::error!("SPA index.html unreadable: {err}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `Cache-Control` for files served straight from the SPA dir by `ServeDir`.
///
/// Hashed `/assets/*` files are immutable by construction. Everything else
/// (icons, manifest.json) gets `no-store`: the epoch mtimes make
/// `Last-Modified` revalidation useless (permanent 304 even after content
/// changes), and the files are small enough to refetch on a LAN.
async fn static_cache_policy(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let is_api = path == "/api" || path.starts_with("/api/");
    let is_asset = path.starts_with("/assets/");
    let mut res = next.run(req).await;
    if !is_api && !res.headers().contains_key(header::CACHE_CONTROL) {
        let policy = if is_asset
            && (res.status().is_success() || res.status() == StatusCode::NOT_MODIFIED)
        {
            "public, max-age=31536000, immutable"
        } else {
            "no-store"
        };
        res.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
    }
    res
}

/// Bind `addr` and serve the API (and SPA, if `spa_dir` is set) until shutdown.
pub async fn serve(
    addr: SocketAddr,
    store: Arc<StateStore>,
    control: Arc<dyn PodControl>,
    spa_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    serve_with_vitals(addr, store, control, spa_dir, None).await
}

/// [`serve`] plus a vitals history store backing `/metrics/vitals*`.
pub async fn serve_with_vitals(
    addr: SocketAddr,
    store: Arc<StateStore>,
    control: Arc<dyn PodControl>,
    spa_dir: Option<PathBuf>,
    vitals: Option<Arc<podd_core::biometrics::VitalsStore>>,
) -> anyhow::Result<()> {
    serve_full(addr, store, control, spa_dir, vitals, None).await
}

/// [`serve_with_vitals`] plus the update agent backing `/updates*`.
pub async fn serve_full(
    addr: SocketAddr,
    store: Arc<StateStore>,
    control: Arc<dyn PodControl>,
    spa_dir: Option<PathBuf>,
    vitals: Option<Arc<podd_core::biometrics::VitalsStore>>,
    updates: Option<Arc<dyn UpdateOps>>,
) -> anyhow::Result<()> {
    let app = router_full(store, control, spa_dir, vitals, updates);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log::info!("api listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod cors_tests {
    use super::origin_allowed;
    use axum::http::HeaderValue;

    fn allowed(origin: &str) -> bool {
        origin_allowed(&HeaderValue::from_str(origin).unwrap())
    }

    #[test]
    fn localhost_and_loopback() {
        assert!(allowed("http://localhost"));
        assert!(allowed("http://localhost:3000"));
        assert!(allowed("http://127.0.0.1:8080"));
        assert!(allowed("http://127.1.2.3"));
        assert!(allowed("http://[::1]:3000"));
        assert!(allowed("http://[::1]"));
    }

    #[test]
    fn full_rfc1918_ranges() {
        // issue #30: only 10.0.x and 172.16.x used to pass
        assert!(allowed("http://10.1.2.3"));
        assert!(allowed("http://10.42.0.7:8080"));
        assert!(allowed("http://172.20.1.2"));
        assert!(allowed("http://172.31.255.1"));
        assert!(allowed("http://192.168.0.109:5173"));
        assert!(allowed("http://169.254.10.20"));
    }

    #[test]
    fn public_origins_rejected() {
        assert!(!allowed("http://172.32.0.1"));
        assert!(!allowed("http://11.0.0.1"));
        assert!(!allowed("http://192.169.0.1"));
        assert!(!allowed("https://example.com"));
        assert!(!allowed("https://8.8.8.8"));
        assert!(!allowed("http://[2001:db8::1]:80"));
    }
}
