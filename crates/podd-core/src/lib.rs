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
pub mod health;
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
use crate::health::HealthRegistry;
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
    let (health, health_rx) = HealthRegistry::new();

    let shared = Shared {
        status: status_rx,
        health: health_rx,
        commands: cmd_tx,
    };

    let fut = run_inner(config_path, Arc::new(status_tx), health, cmd_rx, dry_run);
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

/// First non-empty (trimmed) line among `candidates`, else `"unknown"`.
fn detect_device_label<'a>(candidates: impl IntoIterator<Item = &'a str>) -> String {
    candidates
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Apply a "Prime daily?" change (UI settings) to the live config.
///
/// Deliberately identical to the MQTT `set_prime` path
/// ([`config::mqtt::handle_action`]): update the config watch so the managers
/// pick it up, then persist `config.ron` so it survives a restart. A no-op
/// change is dropped — sending on the watch resets the frozen manager's manual
/// overrides, which a settings save must not do gratuitously.
async fn apply_prime_daily(
    config_tx: &watch::Sender<Config>,
    config_path: &str,
    enabled: bool,
    time: jiff::civil::Time,
) {
    let mut cfg = config_tx.borrow().clone();
    if cfg.prime_enabled == enabled && cfg.prime == time {
        return;
    }
    cfg.prime_enabled = enabled;
    cfg.prime = time;
    log::info!(
        "Set daily prime to {} (enabled={enabled})",
        time.strftime("%H:%M")
    );
    if let Err(e) = config_tx.send(cfg.clone()) {
        log::error!("Error sending to config watch channel: {e}");
        return;
    }
    if let Err(e) = cfg.save(config_path).await {
        log::error!("Failed to save config: {e}");
    }
}

/// Apply a per-side away-mode change (UI settings page) to the live config.
/// Same shape as [`apply_prime_daily`]: no-op changes are dropped so a
/// settings save doesn't gratuitously reset manual overrides.
async fn apply_away_mode(
    config_tx: &watch::Sender<Config>,
    config_path: &str,
    away: config::AwayMode,
) {
    let mut cfg = config_tx.borrow().clone();
    if cfg.away_mode == away {
        return;
    }
    cfg.away_mode = away;
    log::info!(
        "Set away mode: left={} right={}",
        away.left,
        away.right
    );
    if let Err(e) = config_tx.send(cfg.clone()) {
        log::error!("Error sending to config watch channel: {e}");
        return;
    }
    if let Err(e) = cfg.save(config_path).await {
        log::error!("Failed to save config: {e}");
    }
}

/// Apply a timezone change (UI settings page) to the live config. The API
/// layer already validated the IANA name; re-parse defensively anyway.
async fn apply_timezone(config_tx: &watch::Sender<Config>, config_path: &str, iana: &str) {
    let tz = match jiff::tz::TimeZone::get(iana) {
        Ok(tz) => tz,
        Err(e) => {
            log::error!("SetTimezone: unknown timezone {iana:?}: {e}");
            return;
        }
    };
    let mut cfg = config_tx.borrow().clone();
    if cfg.timezone == tz {
        return;
    }
    cfg.timezone = tz;
    log::info!("Set timezone to {iana}");
    if let Err(e) = config_tx.send(cfg.clone()) {
        log::error!("Error sending to config watch channel: {e}");
        return;
    }
    if let Err(e) = cfg.save(config_path).await {
        log::error!("Failed to save config: {e}");
    }
}

/// Route a command to the manager that owns it. System-level commands and
/// not-yet-mapped ones are logged (dry-run) here.
async fn dispatch_commands(
    mut cmd_rx: mpsc::Receiver<Command>,
    frozen_tx: mpsc::Sender<Command>,
    sensor_tx: mpsc::Sender<Command>,
    config_tx: watch::Sender<Config>,
    config_path: Arc<str>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        match &cmd {
            Command::SetTargetTempF { .. } | Command::SetPower { .. } | Command::Prime => {
                if frozen_tx.send(cmd).await.is_err() {
                    log::warn!("frozen command channel closed; dropping command");
                }
            }
            // Config edits, not manager ops: settings-page bridges.
            Command::SetPrimeDaily { enabled, time } => {
                apply_prime_daily(&config_tx, &config_path, *enabled, *time).await;
            }
            Command::SetAwayMode { left, right } => {
                apply_away_mode(
                    &config_tx,
                    &config_path,
                    config::AwayMode {
                        left: *left,
                        right: *right,
                    },
                )
                .await;
            }
            Command::SetTimezone { iana } => {
                apply_timezone(&config_tx, &config_path, iana).await;
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
    health: HealthRegistry,
    cmd_rx: mpsc::Receiver<Command>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let config_path = config_path.as_path();
    log::info!("Starting {NAME} v{VERSION}...");
    if dry_run {
        log::warn!("dry_run=true: MCU control writes are LOGGED, not sent (safe telemetry mode)");
    }

    // Device label for MQTT topics + the HA discovery node id. Eight's `sewer`
    // writes the stock path (present on L1 installs); the clean-room image
    // doesn't have it, so fall back to the hostname instead of "unknown" (#79).
    let device_label = detect_device_label([
        "/home/dac/app/sewer/device-label",
        "/etc/hostname",
        "/proc/sys/kernel/hostname",
    ]);

    // read config
    let config_path_str = config_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("config path is not valid UTF-8: {config_path:?}"))?;
    let config_path_arc: Arc<str> = Arc::from(config_path_str);
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
    tokio::spawn(dispatch_commands(
        cmd_rx,
        frozen_cmd_tx,
        sensor_cmd_tx,
        config_tx.clone(),
        config_path_arc.clone(),
    ));

    // reset the STM32s via the PCAL6416A I2C expander, then hand the bus to the LED
    let mut resetter = ResetController::new(&device.i2c_bus, device.pcal6416a_addr)
        .map_err(|e| anyhow::anyhow!("failed to init ResetController: {e}"))?;
    resetter
        .reset_subsystems()
        .await
        .map_err(|e| anyhow::anyhow!("failed to reset subsystems: {e}"))?;
    let led = IS31FL3194Controller::new_with_addr(resetter.take(), device.led_addr);

    let (calibrate_tx, calibrate_rx) = mpsc::channel(32);

    let mut mqtt_man = MqttManager::new(
        config_tx.clone(),
        config_rx.clone(),
        calibrate_tx,
        device_label,
        config_path_arc.clone(),
        health.clone(),
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
            health.clone(),
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
            health.clone(),
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

#[cfg(test)]
mod device_label_tests {
    use super::detect_device_label;

    #[test]
    fn falls_back_through_missing_files_to_unknown() {
        assert_eq!(detect_device_label(["/nonexistent/a", "/nonexistent/b"]), "unknown");
    }

    #[test]
    fn kernel_hostname_is_a_working_fallback() {
        // /proc/sys/kernel/hostname exists on any Linux host, so the chain
        // used in run_inner can't reach "unknown" there.
        let label = detect_device_label(["/nonexistent/a", "/proc/sys/kernel/hostname"]);
        assert_ne!(label, "unknown");
        assert!(!label.is_empty());
        // trimmed: the proc file ends with a newline
        assert_eq!(label, label.trim());
    }
}
