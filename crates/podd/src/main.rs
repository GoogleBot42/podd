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
//! Today this binary is a thin entry point: it parses a config path, inits
//! logging, and hands off to [`podd_core::run`]. The api/schedule/update
//! layers land here as they are built.

use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    // config path: first positional arg, default `./config.ron`
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./config.ron"));

    log::info!("podd starting (config: {})", config_path.display());

    podd_core::run(&config_path).await
}
