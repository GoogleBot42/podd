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

pub mod bus;
pub mod config;
pub mod frozen;
pub mod ha_discovery;
pub mod led;
pub mod mqtt;
pub mod reset;
pub mod sensor;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::{Config, Cover};
use tokio::sync::{mpsc, watch};

use crate::bus::{Command, DeviceSnapshot, Shared, StatusTx};
use crate::{led::IS31FL3194Controller, mqtt::MqttManager, reset::ResetController};

pub const NAME: &str = "podd";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Command channel depth (api/scheduler -> managers).
const COMMAND_QUEUE: usize = 64;

/// Create the state bus and return the consumer-facing [`Shared`] handle
/// alongside the long-lived future that drives the control core.
///
/// The channels are created synchronously and eagerly, so a caller (e.g. the
/// `podd` binary) can hand [`Shared`] to the `api` layer *before* any hardware
/// is touched. All hardware init (STM32 reset, LED, MQTT connect) and the
/// managers' `select!` loop live inside the returned future, which resolves
/// with an error if the hardware is missing (a dev box has no UARTs — that is
/// expected and non-fatal to *building*/wiring the stack).
///
/// `dry_run` gates MCU *writes*: when true (the default for the live cutover),
/// command frames are logged rather than sent. Telemetry publishing is always
/// live (read-only, safe).
pub fn start(
    config_path: PathBuf,
    dry_run: bool,
) -> (Shared, impl Future<Output = anyhow::Result<()>>) {
    let (status_tx, status_rx) = watch::channel(DeviceSnapshot::default());
    let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_QUEUE);

    let shared = Shared {
        status: status_rx,
        commands: cmd_tx,
    };

    let fut = run_inner(config_path, Arc::new(status_tx), cmd_rx, dry_run);
    (shared, fut)
}

/// Runs the control core: load config, reset the STM32 subsystems, then drive
/// the Frozen + Sensor subsystems and the MQTT manager in a `select!`.
///
/// Reproduces opensleep's `main()` startup. Whichever of the three long-lived
/// futures returns first brings the process down (systemd is expected to
/// restart it).
///
/// Device paths/bauds/I2C bus/addresses come from the config's `device`
/// section (see [`config::DeviceConfig`]); an absent section falls back to the
/// historical hard-coded defaults.
///
/// Convenience wrapper over [`start`] for callers that don't need the [`Shared`]
/// bus (the dropped command sender simply disables the command path). Defaults
/// to `dry_run = true`.
pub async fn run(config_path: &Path) -> anyhow::Result<()> {
    let (_shared, fut) = start(config_path.to_path_buf(), true);
    fut.await
}

/// Route a command to the manager that owns it. System-level commands and
/// not-yet-mapped ones are logged (dry-run) here.
async fn dispatch_commands(
    mut cmd_rx: mpsc::Receiver<Command>,
    frozen_tx: mpsc::Sender<Command>,
    sensor_tx: mpsc::Sender<Command>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        match &cmd {
            Command::SetTargetTempF { .. } | Command::SetPower { .. } | Command::Prime => {
                if frozen_tx.send(cmd).await.is_err() {
                    log::warn!("frozen command channel closed; dropping command");
                }
            }
            Command::ClearAlarm { .. } | Command::FireAlarm(_) => {
                if sensor_tx.send(cmd).await.is_err() {
                    log::warn!("sensor command channel closed; dropping command");
                }
            }
            // Not yet mapped to a manager op — plumbing only.
            Command::SetSettingsCbor(bytes) => {
                log::warn!(
                    "SetSettingsCbor({} bytes) not yet applied // TODO(live-cutover)",
                    bytes.len()
                );
            }
            Command::Reboot | Command::Update | Command::Execute { .. } => {
                log::warn!("system command {cmd:?} not yet implemented // TODO(live-cutover)");
            }
        }
    }
    log::info!("command dispatcher exiting (all senders dropped)");
}

async fn run_inner(
    config_path: PathBuf,
    status_tx: StatusTx,
    cmd_rx: mpsc::Receiver<Command>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let config_path = config_path.as_path();
    log::info!("Starting {NAME} v{VERSION}...");
    if dry_run {
        log::warn!("dry_run=true: MCU control writes are LOGGED, not sent (safe telemetry mode)");
    }

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
    // Hardware wiring (device paths/bauds/I2C). Borrowed by the subsystem tasks
    // below, so keep it alive for the whole `run` scope.
    let device = config.device.clone();
    log::info!(
        "Device: frozen {}@{}, sensor {}@{}(bl)/{}(fw), i2c {}",
        device.frozen_port,
        device.frozen_baud,
        device.sensor_port,
        device.sensor_bootloader_baud,
        device.sensor_firmware_baud,
        device.i2c_bus,
    );
    let (config_tx, config_rx) = watch::channel(config.clone());
    log::info!(
        "Using timezone: {}",
        config.timezone.iana_name().unwrap_or("ERROR")
    );

    // Seed the snapshot's static/config-derived fields (cover version).
    let cover_version = match device.cover {
        Some(Cover::Pod3) => "Pod 3",
        Some(Cover::Pod4) => "Pod 4",
        None => "unknown",
    };
    status_tx.send_modify(|s| {
        s.cover = device.cover;
        s.cover_version = cover_version.to_string();
    });

    // Fan the single public command channel out to per-manager channels.
    let (frozen_cmd_tx, frozen_cmd_rx) = mpsc::channel(COMMAND_QUEUE);
    let (sensor_cmd_tx, sensor_cmd_rx) = mpsc::channel(COMMAND_QUEUE);
    tokio::spawn(dispatch_commands(cmd_rx, frozen_cmd_tx, sensor_cmd_tx));

    // reset the STM32s via the PCAL6416A I2C expander, then hand the bus to the LED
    let mut resetter = ResetController::new(&device.i2c_bus, device.pcal6416a_addr)
        .map_err(|e| anyhow::anyhow!("failed to init ResetController: {e}"))?;
    resetter
        .reset_subsystems()
        .await
        .map_err(|e| anyhow::anyhow!("failed to reset subsystems: {e}"))?;
    let led = IS31FL3194Controller::new_with_addr(resetter.take(), device.led_addr);

    let (calibrate_tx, calibrate_rx) = mpsc::channel(32);

    let config_path_arc: Arc<str> = Arc::from(config_path_str);
    let mut mqtt_man = MqttManager::new(
        config_tx.clone(),
        config_rx.clone(),
        calibrate_tx,
        device_label,
        config_path_arc.clone(),
    );

    // MQTT must NEVER gate the hardware. Give the broker a brief chance to
    // connect, but do not block the frozen/sensor managers if it is unreachable
    // — it keeps retrying concurrently via `mqtt_man.run()` in the select! below,
    // and telemetry to the api/StateBus flows regardless of MQTT.
    match tokio::time::timeout(std::time::Duration::from_secs(3), mqtt_man.wait_for_conn()).await {
        Ok(Ok(())) => log::info!("MQTT connected"),
        Ok(Err(())) => log::warn!("MQTT connect failed (continuing without it)"),
        Err(_) => log::warn!("MQTT not connected within 3s (continuing; retrying in background)"),
    }

    // Any manager ending — Ok or Err — means the control core is no longer
    // driving the hardware, so the whole process must die and let systemd
    // restart it. Returning Ok here would leave the api task serving stale
    // state while every command is dropped (observed live: transient "Sensor
    // not responding" killed the core but podd kept answering HTTP).
    let failure: anyhow::Error = tokio::select! {
        res = frozen::run(
            &device.frozen_port,
            device.frozen_baud,
            config_rx.clone(),
            led,
            mqtt_man.client.clone(),
            status_tx.clone(),
            frozen_cmd_rx,
            dry_run,
        ) => {
            match res {
                Ok(_) => anyhow::anyhow!("Frozen task unexpectedly exited"),
                Err(e) => anyhow::anyhow!("Frozen task failed: {e}"),
            }
        }

        // supervise() retries the sensor internally forever — a flaky sensor
        // MCU must not take the frozen/TEC control loop down with it. This arm
        // completing at all is therefore unexpected.
        res = sensor::supervise(
            &device.sensor_port,
            device.sensor_bootloader_baud,
            device.sensor_firmware_baud,
            config_tx,
            config_rx,
            config_path_arc,
            calibrate_rx,
            mqtt_man.client.clone(),
            status_tx.clone(),
            sensor_cmd_rx,
            dry_run,
        ) => {
            match res {
                Ok(_) => anyhow::anyhow!("Sensor supervisor unexpectedly exited"),
                Err(e) => anyhow::anyhow!("Sensor supervisor failed: {e}"),
            }
        }

        _ = mqtt_man.run() => {
            anyhow::anyhow!("MQTT manager unexpectedly exited")
        }
    };

    log::error!("{failure}");
    log::info!("Shutting down {NAME}...");
    Err(failure)
}
