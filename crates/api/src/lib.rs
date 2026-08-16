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

pub use control::{Call, MockControl, NotImplemented, PoddControl, PodControl};
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
