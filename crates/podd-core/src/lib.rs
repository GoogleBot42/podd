//! `podd-core` — the opensleep control core, vendored for podd.
//!
//! Vendored/adapted from opensleep (GPL-3.0), <https://github.com/LiamSnow/opensleep>.
//!
//! The `frozen`, `sensor`, `led`, `config`, `mqtt` and `reset` modules are kept
//! ~1:1 with upstream so that upstream fixes can be cherry-picked later. The
//! SoC-agnostic protocol pieces (framing/codec/CRC, packet & command tables,
//! thermostat math) have been factored out into the `pod-proto` crate and are
//! re-used here.
//!
//! Upstream's `src/main.rs` startup has been converted into [`run`].

pub mod config;
pub mod frozen;
pub mod led;
pub mod mqtt;
pub mod reset;
pub mod sensor;

use std::path::Path;

use config::Config;
use tokio::sync::{mpsc, watch};

use crate::{led::IS31FL3194Controller, mqtt::MqttManager, reset::ResetController};

pub const NAME: &str = "podd";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Runs the control core: load config, reset the STM32 subsystems, then drive
/// the Frozen + Sensor subsystems and the MQTT manager in a `select!`.
///
/// Reproduces opensleep's `main()` startup. Whichever of the three long-lived
/// futures returns first brings the process down (systemd is expected to
/// restart it).
///
/// TODO: the device paths/bauds/I2C bus are still hard-coded (see
/// [`frozen::PORT`], [`sensor::PORT`], and `reset.rs`); make them config.
pub async fn run(config_path: &Path) -> anyhow::Result<()> {
    log::info!("Starting {NAME} v{VERSION}...");

    // read device label (best-effort; Eight's `sewer` writes this)
    // TODO: make this path config / drop it
    let device_label = std::fs::read_to_string("/home/dac/app/sewer/device-label")
        .unwrap_or_else(|_| "unknown".to_string());

    // read config
    let config_path_str = config_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("config path is not valid UTF-8: {config_path:?}"))?;
    let config = Config::load(config_path_str).await?;
    log::info!("`{}` loaded", config_path.display());
    let (config_tx, config_rx) = watch::channel(config.clone());
    log::info!(
        "Using timezone: {}",
        config.timezone.iana_name().unwrap_or("ERROR")
    );

    // reset the STM32s via the PCAL6416A I2C expander, then hand the bus to the LED
    let mut resetter =
        ResetController::new().map_err(|e| anyhow::anyhow!("failed to init ResetController: {e}"))?;
    resetter
        .reset_subsystems()
        .await
        .map_err(|e| anyhow::anyhow!("failed to reset subsystems: {e}"))?;
    let led = IS31FL3194Controller::new(resetter.take());

    let (calibrate_tx, calibrate_rx) = mpsc::channel(32);

    let mut mqtt_man = MqttManager::new(
        config_tx.clone(),
        config_rx.clone(),
        calibrate_tx,
        device_label,
    );

    if mqtt_man.wait_for_conn().await.is_err() {
        anyhow::bail!("Fatal error starting MQTT. Shutting down...");
    }

    tokio::select! {
        res = frozen::run(
            frozen::PORT,
            config_rx.clone(),
            led,
            mqtt_man.client.clone()
        ) => {
            match res {
                Ok(_) => log::error!("Frozen task unexpectedly exited"),
                Err(e) => log::error!("Frozen task failed: {e}"),
            }
        }

        res = sensor::run(
            sensor::PORT,
            config_tx,
            config_rx,
            calibrate_rx,
            mqtt_man.client.clone()
        ) => {
            match res {
                Ok(_) => log::error!("Sensor task unexpectedly exited"),
                Err(e) => log::error!("Sensor task failed: {e}"),
            }
        }

        _ = mqtt_man.run() => {
            log::error!("MQTT manager unexpectedly exited");
        }
    }

    log::info!("Shutting down {NAME}...");
    Ok(())
}
