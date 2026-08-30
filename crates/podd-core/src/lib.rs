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

pub mod alarm;
pub mod biometrics;
pub mod bus;
pub mod config;
pub mod frozen;
pub mod ha_discovery;
pub mod health;
pub mod led;
pub mod mqtt;
pub mod reset;
pub mod schedule;
pub mod sensor;
pub mod settings;
pub mod version;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::{Config, Cover};
use tokio::sync::{mpsc, watch};

use crate::bus::{Command, DeviceSnapshot, MqttSnapshot, Shared, StatusTx};
use crate::health::HealthRegistry;
use crate::{led::IS31FL3194Controller, mqtt::MqttManager, reset::ResetController};

pub const NAME: &str = "podd";
pub use crate::version::{GIT_REV, VERSION};

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
    // Broker settings for the api layer (#18). Seeded with the "not
    // configured" placeholder and replaced by `run_inner` the moment
    // `config.ron` is parsed.
    let (mqtt_state_tx, mqtt_state_rx) = watch::channel(MqttSnapshot::default());

    // Vitals history store, next to the config like settings/schedules. A
    // pre-NTP clock (epoch ~0) makes the prune cutoff negative — a no-op,
    // never an over-prune.
    let vitals = match biometrics::VitalsStore::open(sibling_path(&config_path, "vitals.jsonl")) {
        Ok(store) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if let Err(e) = store.prune(now, biometrics::RETENTION_DAYS) {
                log::warn!("vitals store prune failed: {e}");
            }
            Some(Arc::new(store))
        }
        Err(e) => {
            log::warn!("vitals store unavailable (biometrics disabled): {e}");
            None
        }
    };

    let shared = Shared {
        status: status_rx,
        health: health_rx,
        mqtt: mqtt_state_rx,
        commands: cmd_tx,
        vitals: vitals.clone(),
    };

    let fut = run_inner(
        config_path,
        Arc::new(status_tx),
        health,
        cmd_rx,
        mqtt_state_tx,
        dry_run,
        vitals,
    );
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

/// Where the api layer's JSON documents live: next to `config.ron`.
///
/// Deliberately the same derivation the `podd` binary uses for the API layer's
/// `StateStore` (`crates/podd/src/main.rs`) — the API owns writing these files,
/// podd-core only ever reads them, and the two must agree on which file it is.
fn sibling_path(config_path: &Path, name: &str) -> PathBuf {
    config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(name)
}

fn schedules_path(config_path: &Path) -> PathBuf {
    sibling_path(config_path, "schedules.json")
}

fn settings_path(config_path: &Path) -> PathBuf {
    sibling_path(config_path, "settings.json")
}

/// Read `schedules.json`, or the all-disabled default if it is missing or
/// unreadable/corrupt (matching `api::StateStore`'s load-or-default, so both
/// halves of the daemon start from the same document).
///
/// The default leaves every weekday disabled, which means the legacy
/// `config.ron` profile keeps driving the bed — the safe direction for a
/// garbled file.
async fn load_schedules(path: &Path) -> schedule::Schedules {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::info!(
                "{} not present; the config.ron profile drives both sides",
                path.display()
            );
            return schedule::Schedules::default();
        }
        Err(e) => {
            log::warn!("failed to read {}: {e}; using default", path.display());
            return schedule::Schedules::default();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        log::warn!("failed to parse {}: {e}; using default", path.display());
        schedule::Schedules::default()
    })
}

/// Read `settings.json`, or the free-sleep defaults if it is missing or
/// unreadable/corrupt — the same load-or-default `api::StateStore` performs,
/// so the settings the UI shows and the ones the daemon acts on can't diverge
/// on a garbled file.
async fn load_settings(path: &Path) -> settings::Settings {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("failed to read {}: {e}; using defaults", path.display());
            }
            return settings::Settings::default();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        log::warn!("failed to parse {}: {e}; using defaults", path.display());
        settings::Settings::default()
    })
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
///
/// Returns whether the live config actually changed, so the caller knows
/// whether the retained MQTT state topic needs republishing (#106).
async fn apply_prime_daily(
    config_tx: &watch::Sender<Config>,
    config_path: &str,
    enabled: bool,
    time: jiff::civil::Time,
) -> bool {
    let mut cfg = config_tx.borrow().clone();
    if cfg.prime_enabled == enabled && cfg.prime == time {
        return false;
    }
    cfg.prime_enabled = enabled;
    cfg.prime = time;
    log::info!(
        "Set daily prime to {} (enabled={enabled})",
        time.strftime("%H:%M")
    );
    if let Err(e) = config_tx.send(cfg.clone()) {
        log::error!("Error sending to config watch channel: {e}");
        return false;
    }
    if let Err(e) = cfg.save(config_path).await {
        log::error!("Failed to save config: {e}");
    }
    true
}

/// Apply a per-side away-mode change (UI settings page) to the live config.
/// Same shape as [`apply_prime_daily`]: no-op changes are dropped so a
/// settings save doesn't gratuitously reset manual overrides.
async fn apply_away_mode(
    config_tx: &watch::Sender<Config>,
    config_path: &str,
    away: config::AwayMode,
) -> bool {
    let mut cfg = config_tx.borrow().clone();
    if cfg.away_mode == away {
        return false;
    }
    cfg.away_mode = away;
    log::info!(
        "Set away mode: left={} right={}",
        away.left,
        away.right
    );
    if let Err(e) = config_tx.send(cfg.clone()) {
        log::error!("Error sending to config watch channel: {e}");
        return false;
    }
    if let Err(e) = cfg.save(config_path).await {
        log::error!("Failed to save config: {e}");
    }
    true
}

/// Apply a timezone change (UI settings page) to the live config. The API
/// layer already validated the IANA name; re-parse defensively anyway.
async fn apply_timezone(config_tx: &watch::Sender<Config>, config_path: &str, iana: &str) -> bool {
    let tz = match jiff::tz::TimeZone::get(iana) {
        Ok(tz) => tz,
        Err(e) => {
            log::error!("SetTimezone: unknown timezone {iana:?}: {e}");
            return false;
        }
    };
    let mut cfg = config_tx.borrow().clone();
    if cfg.timezone == tz {
        return false;
    }
    cfg.timezone = tz;
    log::info!("Set timezone to {iana}");
    if let Err(e) = config_tx.send(cfg.clone()) {
        log::error!("Error sending to config watch channel: {e}");
        return false;
    }
    if let Err(e) = cfg.save(config_path).await {
        log::error!("Failed to save config: {e}");
    }
    true
}

/// An [`rumqttc::AsyncClient`] whose requests are drained and thrown away.
///
/// Used when `mqtt.enabled` is false (#18): the frozen/sensor managers and the
/// command dispatcher all take a client unconditionally, and the alternative —
/// an `Option<AsyncClient>` threaded through every publish site — would touch
/// the telemetry hot path for no behavioural gain. Draining (rather than
/// dropping the receiver) matters: a dropped receiver makes every publish
/// return an error, which `publish_guaranteed_wait` would log once per
/// telemetry tick.
fn discarding_mqtt_client() -> rumqttc::AsyncClient {
    let (tx, rx) = flume::bounded::<rumqttc::Request>(16);
    tokio::spawn(async move { while rx.recv_async().await.is_ok() {} });
    rumqttc::AsyncClient::from_senders(tx)
}

/// Keep the bus's [`MqttSnapshot`] in step with the live config, so a settings
/// save is reflected by `GET /api/mqtt` without the api layer ever reading
/// `config.ron` (or the password) itself.
async fn mirror_mqtt_state(
    mut config_rx: watch::Receiver<Config>,
    mqtt_state_tx: watch::Sender<MqttSnapshot>,
) {
    while config_rx.changed().await.is_ok() {
        let snap = MqttSnapshot::from(&config_rx.borrow_and_update().mqtt);
        if *mqtt_state_tx.borrow() != snap {
            let _ = mqtt_state_tx.send(snap);
        }
    }
    log::info!("config watch closed; mqtt-state mirror exiting");
}

/// Apply an MQTT broker-settings change (UI Settings → MQTT, #18) to the live
/// config. Same shape as [`apply_timezone`], with two rules of its own:
///
/// * only `cfg.mqtt` is touched — the rest of `config.ron` (profile, alarm,
///   presence, device) round-trips through load/save untouched, which
///   `config_command_tests::mqtt_edit_leaves_the_rest_of_the_config_alone`
///   pins;
/// * `update.password == None` keeps the stored password, so the UI can change
///   a port without ever handling the secret. Nothing here logs it.
///
/// The broker *connection* is built once at startup, so a change only takes
/// effect on the next podd restart — said plainly in the log line and in the
/// UI copy rather than pretended away.
async fn apply_mqtt(
    config_tx: &watch::Sender<Config>,
    config_path: &str,
    update: bus::MqttUpdate,
) -> bool {
    let mut cfg = config_tx.borrow().clone();
    let next = config::MqttConfig {
        enabled: update.enabled,
        server: update.server,
        port: update.port,
        user: update.user,
        password: update.password.unwrap_or_else(|| cfg.mqtt.password.clone()),
    };
    if cfg.mqtt == next {
        return false;
    }
    log::info!(
        "Set MQTT broker to {}:{} (user {:?}, enabled={}); restart podd to reconnect",
        next.server,
        next.port,
        next.user,
        next.enabled,
    );
    cfg.mqtt = next;
    if let Err(e) = config_tx.send(cfg.clone()) {
        log::error!("Error sending to config watch channel: {e}");
        return false;
    }
    if let Err(e) = cfg.save(config_path).await {
        log::error!("Failed to save config: {e}");
    }
    true
}

/// Republish the retained MQTT config-state topics a command just invalidated.
///
/// Config changes arriving over MQTT republish inline in
/// [`config::mqtt::handle_action`]; the API command path had no such hook, so
/// e.g. `opensleep/state/config/prime` stayed stale after a UI settings save
/// until the next broker reconnect (#106). State publishing only — nothing
/// here actuates.
///
/// Spawned, never awaited: [`publish_guaranteed_wait`](crate::mqtt::publish_guaranteed_wait)
/// can block for seconds while the broker is down, and the dispatcher must
/// stay responsive for the commands behind this one (alarm dismissal included).
fn republish_config_state(
    client: &rumqttc::AsyncClient,
    topics: &'static [config::mqtt::ConfigStateTopic],
    config_tx: &watch::Sender<Config>,
) {
    let mut client = client.clone();
    let cfg = config_tx.borrow().clone();
    tokio::spawn(async move {
        config::mqtt::republish_config_state(&mut client, topics, &cfg).await;
    });
}

/// Reboot the device via systemd. Gated on `dry_run` like every actuation: a
/// dev box running the daemon (or `cargo test`) must never reboot itself.
async fn reboot_device(dry_run: bool) {
    if dry_run {
        log::warn!("[dry-run] would reboot the device (systemctl reboot)");
        return;
    }
    log::warn!("Rebooting the device (systemctl reboot)");
    match tokio::process::Command::new("systemctl")
        .arg("reboot")
        .status()
        .await
    {
        Ok(status) if status.success() => {}
        Ok(status) => log::error!("systemctl reboot exited with {status}"),
        Err(e) => log::error!("failed to run systemctl reboot: {e}"),
    }
}

/// Minimum process uptime before the daily reboot may fire. After a reboot the
/// system is back inside the ±30 s trigger window (boot takes ~1 min), so an
/// uptime guard is what breaks the reboot→boot→reboot loop.
const REBOOT_MIN_UPTIME: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// The daily-reboot scheduler (settings.json `rebootDaily`, #106): reboots the
/// device one hour before the daily prime time — free-sleep's rule, quoted in
/// the UI's "Reboot once a day" copy. The prime time and timezone come from
/// the live config (the settings page bridges them there), the enable flag
/// from the settings watch.
///
/// Fires only when the clock is NTP-synced: with no RTC battery the boot clock
/// is a restored pre-shutdown timestamp (see `sensor::manager`), and rebooting
/// on untrusted wall time could loop or fire at a real bedtime.
async fn run_reboot_scheduler(
    config_rx: watch::Receiver<Config>,
    schedules_rx: watch::Receiver<schedule::Schedules>,
    settings_rx: watch::Receiver<settings::Settings>,
    dry_run: bool,
) {
    let started = tokio::time::Instant::now();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        tick.tick().await;
        if !settings_rx.borrow().reboot_daily || started.elapsed() < REBOOT_MIN_UPTIME {
            continue;
        }
        let (timezone, prime, profile) = {
            let cfg = config_rx.borrow();
            (cfg.timezone.clone(), cfg.prime, cfg.profile.clone())
        };
        let at = settings::reboot_time(prime);
        let now_zoned = jiff::Timestamp::now().to_zoned(timezone);
        if !settings::in_daily_window(now_zoned.time(), at) {
            continue;
        }
        if !sensor::manager::clock_is_synced() {
            log::warn!("daily reboot due ({at}) but the clock is not NTP-synced; skipping");
        } else if alarm_nearby(&profile, &schedules_rx, &settings_rx, &now_zoned) {
            // A reboot inside (or just before) an alarm window loses the
            // task-local dismissal state and holds the alarm behind the
            // post-boot NTP gate — either re-firing a dismissed alarm or
            // swallowing one. Skip today's reboot instead.
            log::warn!("daily reboot due ({at}) but an alarm window is active or imminent; skipping");
        } else {
            log::info!("Daily reboot window reached ({at})");
            reboot_device(dry_run).await;
        }
        // Sleep past the rest of the ±30 s window so one day triggers one
        // reboot attempt (or one dry-run/skip log), not four.
        tokio::time::sleep(std::time::Duration::from_secs(90)).await;
    }
}

/// Is a scheduled alarm ringing now — or due within the next five minutes
/// (a reboot takes ~1 min plus the NTP wait) — on either side?
fn alarm_nearby(
    profile: &config::SidesConfig,
    schedules_rx: &watch::Receiver<schedule::Schedules>,
    settings_rx: &watch::Receiver<settings::Settings>,
    now: &jiff::Zoned,
) -> bool {
    let schedules = schedules_rx.borrow();
    let user_settings = settings_rx.borrow();
    [0i64, 2, 5].iter().any(|mins| {
        let t = now
            .checked_add(jiff::Span::new().minutes(*mins))
            .unwrap_or_else(|_| now.clone());
        [pod_proto::packet::BedSide::Left, pod_proto::packet::BedSide::Right]
            .iter()
            .any(|side| {
                alarm::resolve_for_side(profile, &schedules, &user_settings, side, &t).is_some()
            })
    })
}

/// Route a command to the manager that owns it. System-level commands and
/// not-yet-mapped ones are logged (dry-run) here.
///
/// `mqtt` is used only to republish retained config-state topics after a
/// config-editing command (#106) — see [`republish_config_state`].
#[allow(clippy::too_many_arguments)]
async fn dispatch_commands(
    mut cmd_rx: mpsc::Receiver<Command>,
    frozen_tx: mpsc::Sender<Command>,
    sensor_tx: mpsc::Sender<Command>,
    config_tx: watch::Sender<Config>,
    config_path: Arc<str>,
    schedules_tx: watch::Sender<schedule::Schedules>,
    settings_tx: watch::Sender<settings::Settings>,
    mqtt: rumqttc::AsyncClient,
    dry_run: bool,
) {
    use config::mqtt::ConfigStateTopic;
    while let Some(cmd) = cmd_rx.recv().await {
        match &cmd {
            Command::SetTargetTempF { .. } | Command::SetPower { .. } | Command::Prime => {
                if frozen_tx.send(cmd).await.is_err() {
                    log::warn!("frozen command channel closed; dropping command");
                }
            }
            // Config edits, not manager ops: settings-page bridges.
            Command::SetPrimeDaily { enabled, time } => {
                if apply_prime_daily(&config_tx, &config_path, *enabled, *time).await {
                    republish_config_state(&mqtt, &[ConfigStateTopic::Prime], &config_tx);
                }
            }
            Command::SetAwayMode { left, right } => {
                let changed = apply_away_mode(
                    &config_tx,
                    &config_path,
                    config::AwayMode {
                        left: *left,
                        right: *right,
                    },
                )
                .await;
                if changed {
                    republish_config_state(&mqtt, &[ConfigStateTopic::AwayMode], &config_tx);
                }
            }
            Command::SetTimezone { iana } => {
                if apply_timezone(&config_tx, &config_path, iana).await {
                    republish_config_state(&mqtt, &[ConfigStateTopic::Timezone], &config_tx);
                }
            }
            // Broker settings (#18). Deliberately NOT republished to MQTT:
            // there is no retained `state/config/mqtt/...` topic, and pushing
            // broker credentials through the broker they authenticate against
            // would be a gift to anyone subscribed to `opensleep/#`.
            Command::SetMqtt(update) => {
                apply_mqtt(&config_tx, &config_path, update.clone()).await;
            }
            // Per-weekday schedule edits: in-memory only. `schedules.json` was
            // already written by the api layer's StateStore (it owns that
            // file), and schedules have no retained MQTT config topic, so
            // there is nothing to persist or republish here.
            Command::SetSchedules(schedules) => {
                if schedules_tx.send((**schedules).clone()).is_err() {
                    log::warn!("schedules watch closed; dropping SetSchedules");
                }
            }
            // Same ownership as SetSchedules: `settings.json` was already
            // persisted by the api layer; only the in-memory copy updates.
            Command::SetSettings(new_settings) => {
                if settings_tx.send((**new_settings).clone()).is_err() {
                    log::warn!("settings watch closed; dropping SetSettings");
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
            // Spawned so a slow/hung systemctl can't stall the dispatcher (the
            // commands queued behind it include alarm dismissal).
            Command::Reboot => {
                tokio::spawn(reboot_device(dry_run));
            }
            Command::Update | Command::Execute { .. } => {
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
    mqtt_state_tx: watch::Sender<MqttSnapshot>,
    dry_run: bool,
    vitals: Option<Arc<biometrics::VitalsStore>>,
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

    // Mirror the broker settings (password-free) onto the bus for the api
    // layer's Settings → MQTT section (#18).
    mqtt_state_tx.send_replace(MqttSnapshot::from(&config.mqtt));
    tokio::spawn(mirror_mqtt_state(config_rx.clone(), mqtt_state_tx));

    // The per-weekday schedule (`schedules.json`, written by the api layer).
    // Read once here; later edits arrive as `Command::SetSchedules`.
    let schedules_file = schedules_path(config_path);
    let schedules = load_schedules(&schedules_file).await;
    let owned: Vec<&str> = schedules
        .sides()
        .iter()
        .filter(|(_, s)| schedule::side_owned(s))
        .map(|&(k, _)| k)
        .collect();
    log::info!(
        "Weekly schedule ({}): drives {}",
        schedules_file.display(),
        if owned.is_empty() {
            "nothing (config.ron profile applies)".to_string()
        } else {
            owned.join(" + ")
        }
    );
    let (schedules_tx, schedules_rx) = watch::channel(schedules);

    // The user settings (`settings.json`, written by the api layer). Read once
    // here; later edits arrive as `Command::SetSettings`. Today the daemon
    // acts on `rebootDaily`; prime/away/timezone behavior still comes from
    // `config.ron` (their settings-page edits are bridged there).
    let settings_file = settings_path(config_path);
    let user_settings = load_settings(&settings_file).await;
    log::info!(
        "User settings ({}): daily reboot {}",
        settings_file.display(),
        if user_settings.reboot_daily {
            "enabled (1 h before prime time)"
        } else {
            "disabled"
        }
    );
    let (settings_tx, settings_rx) = watch::channel(user_settings);

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

    // Fan the single public command channel out to per-manager channels. The
    // dispatcher is spawned further down, once the MQTT client exists (it
    // republishes retained config-state topics after a config edit, #106);
    // commands arriving before then simply queue on `cmd_rx`.
    let (frozen_cmd_tx, frozen_cmd_rx) = mpsc::channel(COMMAND_QUEUE);
    let (sensor_cmd_tx, sensor_cmd_rx) = mpsc::channel(COMMAND_QUEUE);

    // reset the STM32s via the PCAL6416A I2C expander, then hand the bus to the LED
    let mut resetter = ResetController::new(&device.i2c_bus, device.pcal6416a_addr)
        .map_err(|e| anyhow::anyhow!("failed to init ResetController: {e}"))?;
    resetter
        .reset_subsystems()
        .await
        .map_err(|e| anyhow::anyhow!("failed to reset subsystems: {e}"))?;
    let led = IS31FL3194Controller::new_with_addr(resetter.take(), device.led_addr);

    let (calibrate_tx, calibrate_rx) = mpsc::channel(32);

    // `mqtt.enabled: false` (Settings → MQTT, #18) means no broker link at
    // all: no connection attempt, no reconnect backoff, no log spam. The
    // managers still get an `AsyncClient` — threading an `Option` through
    // every publish site would be a far larger change on the telemetry hot
    // path — but it is a discarding one (see [`discarding_mqtt_client`]).
    let (mut mqtt_man, mqtt_client) = if config.mqtt.enabled {
        let man = MqttManager::new(
            config_tx.clone(),
            config_rx.clone(),
            calibrate_tx,
            device_label,
            config_path_arc.clone(),
            health.clone(),
        );
        let client = man.client.clone();
        (Some(man), client)
    } else {
        log::warn!("MQTT disabled in config (mqtt.enabled: false); not connecting to a broker");
        health.report(
            crate::health::MQTT,
            crate::health::Health::NotStarted,
            "disabled in config (mqtt.enabled: false)",
        );
        (None, discarding_mqtt_client())
    };

    tokio::spawn(dispatch_commands(
        cmd_rx,
        frozen_cmd_tx,
        sensor_cmd_tx,
        config_tx.clone(),
        config_path_arc.clone(),
        schedules_tx,
        settings_tx,
        mqtt_client.clone(),
        dry_run,
    ));

    tokio::spawn(run_reboot_scheduler(
        config_rx.clone(),
        schedules_rx.clone(),
        settings_rx.clone(),
        dry_run,
    ));

    // MQTT must NEVER gate the hardware. Give the broker a brief chance to
    // connect, but do not block the frozen/sensor managers if it is unreachable
    // — it keeps retrying concurrently via `mqtt_man.run()` in the select! below,
    // and telemetry to the api/StateBus flows regardless of MQTT.
    if let Some(man) = mqtt_man.as_mut() {
        match tokio::time::timeout(std::time::Duration::from_secs(3), man.wait_for_conn()).await {
            Ok(Ok(())) => log::info!("MQTT connected"),
            Ok(Err(())) => log::warn!("MQTT connect failed (continuing without it)"),
            Err(_) => {
                log::warn!("MQTT not connected within 3s (continuing; retrying in background)")
            }
        }
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
            schedules_rx.clone(),
            settings_rx.clone(),
            led,
            mqtt_client.clone(),
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
            schedules_rx.clone(),
            settings_rx,
            config_path_arc,
            calibrate_rx,
            mqtt_client.clone(),
            status_tx.clone(),
            sensor_cmd_rx,
            health.clone(),
            dry_run,
            vitals,
        ) => {
            match res {
                Ok(_) => anyhow::anyhow!("Sensor supervisor unexpectedly exited"),
                Err(e) => anyhow::anyhow!("Sensor supervisor failed: {e}"),
            }
        }

        // Disabled (`mqtt.enabled: false`): there is no manager to run, so
        // this arm simply never completes.
        _ = async {
            match mqtt_man.as_mut() {
                Some(man) => man.run().await,
                None => std::future::pending().await,
            }
        } => {
            anyhow::anyhow!("MQTT manager unexpectedly exited")
        }
    };

    log::error!("{failure}");
    log::info!("Shutting down {NAME}...");
    Err(failure)
}

/// The "did the live config actually change?" signal that gates the retained
/// MQTT republish (#106). A stale retained topic is only fixed if these report
/// honestly — and a spurious `true` would republish (harmless) while a spurious
/// `false` leaves Home Assistant showing the old value until a reconnect.
#[cfg(test)]
mod config_command_tests {
    use super::*;
    use crate::config::mqtt::ConfigStateTopic;

    /// A scratch config path: `Config::save` writes for real, and the API
    /// command path must not scribble on the repo's example configs.
    fn scratch_path(name: &str) -> String {
        let p = std::env::temp_dir().join(format!("podd-{}-{name}.ron", std::process::id()));
        p.to_str().unwrap().to_string()
    }

    async fn example_config() -> Config {
        // tests run with cwd = crates/podd-core
        Config::load("example_solo.ron").await.unwrap()
    }

    #[tokio::test]
    async fn prime_daily_reports_change_then_no_op() {
        let cfg = example_config().await;
        let (tx, _rx) = watch::channel(cfg.clone());
        let path = scratch_path("prime");
        let new_time: jiff::civil::Time = "05:30".parse().unwrap();
        assert_ne!(cfg.prime, new_time);

        assert!(apply_prime_daily(&tx, &path, cfg.prime_enabled, new_time).await);
        assert_eq!(tx.borrow().prime, new_time);
        assert_eq!(
            ConfigStateTopic::Prime.payload(&tx.borrow()),
            new_time.to_string()
        );

        // re-applying the same values must not republish (or reset the frozen
        // manager's manual overrides)
        assert!(!apply_prime_daily(&tx, &path, cfg.prime_enabled, new_time).await);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn away_mode_reports_change_then_no_op() {
        let cfg = example_config().await;
        let (tx, _rx) = watch::channel(cfg.clone());
        let path = scratch_path("away");
        let both_away = config::AwayMode { left: true, right: true };

        assert!(apply_away_mode(&tx, &path, both_away).await);
        assert_eq!(tx.borrow().away_mode, both_away);
        // the retained topic keeps its whole-bed bool semantics
        assert_eq!(ConfigStateTopic::AwayMode.payload(&tx.borrow()), "true");

        assert!(!apply_away_mode(&tx, &path, both_away).await);

        // one side home => the whole-bed topic goes back to "false"
        let left_only = config::AwayMode { left: true, right: false };
        assert!(apply_away_mode(&tx, &path, left_only).await);
        assert_eq!(ConfigStateTopic::AwayMode.payload(&tx.borrow()), "false");
        let _ = std::fs::remove_file(&path);
    }

    /// The write path behind Settings → MQTT (#18) must round-trip the rest of
    /// `config.ron` untouched. A config write that invented or dropped an
    /// alarm block is exactly the bug that fired a real alarm on a real bed
    /// (2026-07-20), so this asserts the whole document, and the alarm blocks
    /// by name.
    #[tokio::test]
    async fn mqtt_edit_leaves_the_rest_of_the_config_alone() {
        // example_couples.ron has an alarm block on the left side and none on
        // the right — precisely the state a careless write would mangle.
        let cfg = Config::load("example_couples.ron").await.unwrap();
        let (tx, _rx) = watch::channel(cfg.clone());
        let path = scratch_path("mqtt");

        let update = bus::MqttUpdate {
            enabled: true,
            server: "broker.lan".to_string(),
            port: 8883,
            user: "podd".to_string(),
            password: Some("hunter2".to_string()),
        };
        assert!(apply_mqtt(&tx, &path, update.clone()).await);

        // What actually landed on disk, not just what the watch says.
        let saved = Config::load(&path).await.unwrap();
        assert!(saved.mqtt.enabled);
        assert_eq!(saved.mqtt.server, "broker.lan");
        assert_eq!(saved.mqtt.port, 8883);
        assert_eq!(saved.mqtt.user, "podd");
        assert_eq!(saved.mqtt.password, "hunter2");

        // Everything outside the mqtt block is identical to what we loaded.
        let mut without_mqtt = saved.clone();
        without_mqtt.mqtt = cfg.mqtt.clone();
        assert_eq!(
            without_mqtt, cfg,
            "an MQTT edit changed something outside the mqtt block"
        );
        match (&saved.profile, &cfg.profile) {
            (
                config::SidesConfig::Couples { left, right },
                config::SidesConfig::Couples {
                    left: was_left,
                    right: was_right,
                },
            ) => {
                assert_eq!(left.alarm, was_left.alarm);
                assert!(left.alarm.is_some(), "the fixture's alarm must survive");
                assert_eq!(right.alarm, was_right.alarm);
                assert!(right.alarm.is_none(), "no alarm may be invented");
            }
            _ => panic!("expected a couples profile"),
        }

        // An omitted password keeps the stored one: the UI changes a port
        // without ever round-tripping the secret.
        let keep_password = bus::MqttUpdate {
            enabled: false,
            password: None,
            ..update.clone()
        };
        assert!(apply_mqtt(&tx, &path, keep_password.clone()).await);
        let saved = Config::load(&path).await.unwrap();
        assert_eq!(saved.mqtt.password, "hunter2");
        assert!(!saved.mqtt.enabled);

        // Re-applying the same values is a no-op (no watch send, so no
        // gratuitous reset of the frozen manager's manual overrides).
        assert!(!apply_mqtt(&tx, &path, keep_password).await);
        let _ = std::fs::remove_file(&path);
    }

    /// The bus snapshot the api layer reads must never carry the password.
    #[tokio::test]
    async fn mqtt_snapshot_is_password_free() {
        let cfg = Config::load("example_couples.ron").await.unwrap();
        let snap = MqttSnapshot::from(&cfg.mqtt);
        assert_eq!(snap.server, cfg.mqtt.server);
        assert!(snap.password_set);
        assert!(!format!("{snap:?}").contains(&cfg.mqtt.password));

        // ... and neither may the command that carries an edit, whose Debug
        // is hand-written for exactly that reason.
        let update = bus::MqttUpdate {
            enabled: true,
            server: "broker.lan".to_string(),
            port: 1883,
            user: "podd".to_string(),
            password: Some("hunter2".to_string()),
        };
        let rendered = format!("{:?}", Command::SetMqtt(update));
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[tokio::test]
    async fn timezone_reports_change_then_no_op_and_rejects_garbage() {
        let cfg = example_config().await;
        let (tx, _rx) = watch::channel(cfg.clone());
        let path = scratch_path("tz");

        assert!(apply_timezone(&tx, &path, "America/Denver").await);
        assert_eq!(
            ConfigStateTopic::Timezone.payload(&tx.borrow()),
            "America/Denver"
        );

        assert!(!apply_timezone(&tx, &path, "America/Denver").await);
        // an unknown zone changes nothing, so nothing is republished
        assert!(!apply_timezone(&tx, &path, "Mars/Olympus_Mons").await);
        assert_eq!(
            ConfigStateTopic::Timezone.payload(&tx.borrow()),
            "America/Denver"
        );
        let _ = std::fs::remove_file(&path);
    }
}

/// `schedules.json` is read by podd-core but *written* only by the api layer's
/// `StateStore`. If these two disagree about the path — or if a corrupt file
/// were fatal — the daemon would silently drive the bed from a different
/// document than the one the UI edits.
#[cfg(test)]
mod schedules_load_tests {
    use super::*;

    #[test]
    fn path_sits_next_to_the_config() {
        assert_eq!(
            schedules_path(Path::new("/data/podd/config.ron")),
            PathBuf::from("/data/podd/schedules.json")
        );
        // a bare filename means the current directory, like main.rs's base_dir
        assert_eq!(
            schedules_path(Path::new("config.ron")),
            PathBuf::from("./schedules.json")
        );
    }

    #[tokio::test]
    async fn missing_or_corrupt_falls_back_to_all_disabled() {
        let dir = std::env::temp_dir().join(format!("podd-schedules-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let missing = dir.join("nope.json");
        assert_eq!(
            load_schedules(&missing).await,
            schedule::Schedules::default()
        );

        let corrupt = dir.join("corrupt.json");
        std::fs::write(&corrupt, b"{ not json").unwrap();
        assert_eq!(
            load_schedules(&corrupt).await,
            schedule::Schedules::default()
        );

        // and the default is *unowned*: the config.ron profile keeps driving
        let loaded = load_schedules(&corrupt).await;
        assert!(!schedule::side_owned(&loaded.left));
        assert!(!schedule::side_owned(&loaded.right));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_real_document_round_trips() {
        let dir = std::env::temp_dir().join(format!("podd-schedules-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("schedules.json");

        let mut want = schedule::Schedules::default();
        want.left.monday.power.enabled = true;
        want.left.monday.power.on_temperature = 77;
        std::fs::write(&path, serde_json::to_vec(&want).unwrap()).unwrap();

        let got = load_schedules(&path).await;
        assert_eq!(got, want);
        assert!(schedule::side_owned(&got.left));
        assert!(!schedule::side_owned(&got.right));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `settings.json` shares `schedules.json`'s ownership (api writes, podd-core
/// reads) and must fail the same safe direction: missing or corrupt means
/// no schedule overrides and — critically — no daily reboots.
#[cfg(test)]
mod settings_load_tests {
    use super::*;

    #[test]
    fn path_sits_next_to_the_config() {
        assert_eq!(
            settings_path(Path::new("/data/podd/config.ron")),
            PathBuf::from("/data/podd/settings.json")
        );
    }

    #[tokio::test]
    async fn missing_or_corrupt_falls_back_to_the_safe_defaults() {
        let dir = std::env::temp_dir().join(format!("podd-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let missing = dir.join("nope.json");
        let corrupt = dir.join("corrupt.json");
        std::fs::write(&corrupt, b"{ not json").unwrap();

        for path in [&missing, &corrupt] {
            let got = load_settings(path).await;
            assert_eq!(got, settings::Settings::default());
            // the safe direction: no scheduled reboots, no live overrides
            assert!(!got.reboot_daily);
            assert_eq!(got.left.schedule_overrides, Default::default());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_real_document_round_trips() {
        let dir = std::env::temp_dir().join(format!("podd-settings-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        let mut want = settings::Settings::default();
        want.reboot_daily = true;
        want.left.schedule_overrides.alarm.disabled = true;
        std::fs::write(&path, serde_json::to_vec(&want).unwrap()).unwrap();

        assert_eq!(load_settings(&path).await, want);
        let _ = std::fs::remove_dir_all(&dir);
    }
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
