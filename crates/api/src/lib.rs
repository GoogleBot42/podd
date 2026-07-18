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
pub mod state;
pub mod wire;

pub use control::{Call, MockControl, PoddControl, PodControl};
pub use state::{StateStore, StoreConfig};

use axum::http::{HeaderValue, Method};
use axum::routing::{get, post, put};
use axum::Router;
use handlers::AppState;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

/// True for localhost / LAN origins (`192.168.*`, `172.16.*`, `10.0.*`).
fn origin_allowed(origin: &HeaderValue) -> bool {
    let Ok(s) = origin.to_str() else {
        return false;
    };
    // Strip scheme, then take the host portion (before any ':' port).
    let hostport = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))
        .unwrap_or(s);
    let host = hostport.split('/').next().unwrap_or(hostport);
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);

    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host.starts_with("192.168.")
        || host.starts_with("172.16.")
        || host.starts_with("10.0.")
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
    let app_state = AppState { store, control };

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
        .route("/alarm", post(handlers::post_alarm))
        .route("/execute", post(handlers::post_execute))
        .route("/jobs", post(handlers::post_jobs))
        .route(
            "/services",
            get(handlers::get_services).post(handlers::post_services),
        )
        .route("/serverStatus", get(handlers::get_server_status))
        .route("/logs", get(handlers::get_logs))
        .route("/logs/{filename}", get(handlers::get_log_stream))
        .route(
            "/metrics/presence",
            get(handlers::get_presence).post(handlers::post_presence),
        )
        // biometrics — deferred, UI-friendly empties
        .route(
            "/metrics/sleep",
            get(handlers::empty_array),
        )
        .route(
            "/metrics/sleep/{id}",
            put(handlers::sleep_put).delete(handlers::sleep_delete),
        )
        .route("/metrics/vitals", get(handlers::empty_array))
        .route("/metrics/vitals/summary", get(handlers::vitals_summary))
        .route("/metrics/movement", get(handlers::empty_array))
        .fallback(|| async { error::not_found() })
        .with_state(app_state);

    let mut app = Router::new().nest("/api", api);

    app = match spa_dir {
        Some(dir) => {
            let index = dir.join("index.html");
            let serve_dir = ServeDir::new(&dir).fallback(ServeFile::new(index));
            app.fallback_service(serve_dir)
        }
        None => app.fallback(|| async { error::not_found() }),
    };

    app.layer(cors_layer())
}

/// Bind `addr` and serve the API (and SPA, if `spa_dir` is set) until shutdown.
pub async fn serve(
    addr: SocketAddr,
    store: Arc<StateStore>,
    control: Arc<dyn PodControl>,
    spa_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let app = router(store, control, spa_dir);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log::info!("api listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
