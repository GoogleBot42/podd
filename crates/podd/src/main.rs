//! `podd` — the open control daemon for the Eight Sleep Pod.
//!
//! This is a fork-in-progress of opensleep (GPL-3.0). The plan (see
//! `docs/REPLACEMENT_PLAN.md`) is to grow it into the single daemon that
//! replaces Eight's dac/frank/capybara stack:
//!   - the LSP UART layer to the two STM32 MCUs (from opensleep `common/`),
//!   - the Frozen (TEC/pump) and Sensor (presence/temp/HR) subsystems,
//!   - a local thermostat + scheduler + alarms,
//!   - an axum REST/WebSocket API serving the (forked free-sleep) web UI,
//!   - the on-device update agent (Tier-2 atomic release swaps, using
//!     `pod-update` to verify signed manifests).
//!
//! For now it is a compiling stub that reports build/version info; the control
//! core is integrated next, guided by the opensleep source map.

fn main() {
    println!(
        "podd {} — open Eight Sleep Pod firmware (control core integration in progress)",
        env!("CARGO_PKG_VERSION")
    );
    println!("update-manifest schema: v{}", pod_update::SCHEMA_VERSION);
}
