//! `podd` — the open control daemon for the Eight Sleep Pod.
//!
//! This is a fork-in-progress of opensleep (GPL-3.0). The plan (see
//! `docs/REPLACEMENT_PLAN.md`) is to grow it into the single daemon that
//! replaces Eight's dac/frank/capybara stack:
//!   - the LSP UART layer to the two STM32 MCUs (from opensleep `common/`,
//!     now the `pod-proto` crate),
//!   - the Frozen (TEC/pump) and Sensor (presence/temp/HR) subsystems
//!     (the `podd-core` crate),
//!   - a local thermostat + scheduler + alarms,
//!   - an axum REST/WebSocket API serving the (forked free-sleep) web UI,
//!   - the on-device update agent (Tier-2 atomic release swaps, using
//!     `pod-update` to verify signed manifests).
//!
//! This binary now wires the two seams together: it [`start`](podd_core::start)s
//! the control core (which returns the state bus [`Shared`](podd_core::bus::Shared)),
//! builds the `api` layer's live [`StateStore`](api::StateStore) +
//! [`PoddControl`](api::PoddControl) from it, and runs the managers' future and
//! the HTTP server concurrently.
//!
//! MCU control writes stay gated behind `dry_run` (default true): the UI sees
//! **live telemetry** and its commands flow to the managers, but the managers
//! *log* control frames instead of sending them until the live cutover. Set
//! `PODD_DRY_RUN=false` to arm real writes (do this only when podd replaces the
//! stock stack).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use api::{PoddControl, PodControl, StateStore, StoreConfig};

mod hostinfo;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    // App-update trial guard, before ANYTHING else (even config parsing): if a
    // freshly activated release keeps crashing during startup, this counts the
    // boot attempts, rolls the `current` symlink back to the previous release,
    // and exits so systemd respawns into the restored binary (ExecStart
    // re-resolves the symlink). See `pod_updater::trial`.
    if let pod_updater::BootDecision::RolledBack { failed_version } =
        pod_updater::early_boot_guard_from_env()
    {
        log::error!("app release {failed_version} rolled back; exiting for systemd to respawn");
        std::process::exit(1);
    }

    // config path: first positional arg, default `./config.ron`
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./config.ron"));

    // API bind address (default 0.0.0.0:3000) and optional SPA dir.
    let api_addr: SocketAddr = std::env::var("PODD_API_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:3000".to_string())
        .parse()?;
    let spa_dir = std::env::var("PODD_SPA_DIR").ok().map(PathBuf::from);

    // MCU control writes are logged, not sent, unless explicitly armed.
    let dry_run = !matches!(
        std::env::var("PODD_DRY_RUN").ok().as_deref(),
        Some("false") | Some("0")
    );

    // Settings/schedules stay file-backed, next to the config by default.
    let base_dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let store_config = StoreConfig {
        settings_path: Some(base_dir.join("settings.json")),
        schedules_path: Some(base_dir.join("schedules.json")),
    };

    log::info!(
        "podd starting (config: {}, api: {api_addr}, dry_run: {dry_run})",
        config_path.display()
    );

    // Start the control core: creates the bus synchronously (no hardware yet).
    let (shared, core_fut) = podd_core::start(config_path, dry_run);

    // Live status store (fed by the watch) + real control (feeds the mpsc).
    let store = StateStore::from_watch(shared.status.clone(), store_config);
    store.spawn_health_updater(shared.health.clone());
    let control = Arc::new(PoddControl::new(shared.commands.clone())) as Arc<dyn PodControl>;

    // Hub identity + WiFi strength for /api/deviceStatus (host facts, not MCU).
    hostinfo::spawn(store.clone());

    // The on-device update agent: polls a signed release channel and applies
    // Tier-2 (app) updates atomically with a health-checked rollback; Tier-1
    // (OS) / Tier-3 (MCU) stay behind dry-run gates. Configured from the
    // environment (see `pod_updater::UpdaterConfig::from_env`); default is
    // enabled + manual + dry-run, so on a dev box (no sources, no release dir)
    // it simply idles and never tears the process down.
    let updater_fut = pod_updater::run_from_env();

    // Run the managers, the HTTP server, and the update agent together;
    // whichever fails first brings the process down (systemd restarts it). On a
    // dev box with no UARTs, `core_fut` errors out here — expected, not a panic.
    // `updater_fut` only ever resolves on shutdown, never on a transient error.
    tokio::try_join!(
        core_fut,
        api::serve_with_vitals(api_addr, store, control, spa_dir, shared.vitals.clone()),
        updater_fut,
    )?;
    Ok(())
}
