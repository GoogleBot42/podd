//! The state-bus + command seam between `podd-core` and its consumers
//! (the `api` HTTP layer, MQTT, …).
//!
//! Two channels, mirroring the two directions of data flow:
//!
//! * **State fan-out** — a [`tokio::sync::watch`] carrying the latest
//!   [`DeviceSnapshot`]. The Frozen + Sensor managers are the sole producers
//!   (read-only telemetry: they publish from the state they already parse off
//!   the UARTs); `api`/MQTT are peer subscribers. `watch` is latest-value, which
//!   is exactly what a `GET /deviceStatus` poll wants.
//! * **Commands** — a [`tokio::sync::mpsc`] carrying [`Command`]s from `api`
//!   (and, later, schedulers) into the managers. A small dispatcher in
//!   [`crate::run`] fans each command out to the manager that owns it.
//!
//! [`Shared`] bundles the two consumer-facing halves (the `watch::Receiver` and
//! the `mpsc::Sender`) so the daemon can hand one value to `api`.
//!
//! NB: everything here is plumbing. Commands that would *write* setpoints/control
//! frames to an MCU are gated behind a `dry_run` flag (default true) in the
//! managers — see `frozen::manager` / `sensor::manager`. This module defines the
//! wire; the live cutover flips `dry_run` off command-by-command.

use std::sync::Arc;

use jiff::civil::Time;
use tokio::sync::{mpsc, watch};

use crate::config::Cover;
use pod_proto::packet::BedSide;
use pod_proto::sensor::command::AlarmPattern;

/// The producer half of the state fan-out, shared (via `Arc`) between the
/// Frozen and Sensor managers. Both call [`watch::Sender::send_modify`] to
/// update the disjoint fields they own.
pub type StatusTx = Arc<watch::Sender<DeviceSnapshot>>;

/// Per-side device telemetry. Temperatures are °C (podd-core's native unit); the
/// `api` layer converts to °F / "level" on the wire.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SideSnapshot {
    /// Current measured side temperature, °C. `None` until the first reading.
    pub current_temp_c: Option<f64>,
    /// Target side temperature, °C. `None` until a target is known.
    pub target_temp_c: Option<f64>,
    /// Whether the side's target is enabled (actively heating/cooling).
    pub is_on: bool,
    /// Whether an alarm is actively vibrating on this side.
    pub is_alarm_vibrating: bool,
    /// Seconds remaining on the current session (0 = unknown/none; not yet
    /// tracked by the Frozen firmware, reserved for the scheduler).
    pub seconds_remaining: i64,
}

/// A latest-value snapshot of everything the `api` handlers (device status +
/// presence) read. Produced by the managers, consumed by `api`/MQTT.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceSnapshot {
    pub left: SideSnapshot,
    pub right: SideSnapshot,
    /// `true` = water tank present/full.
    pub water_level: bool,
    pub is_priming: bool,
    pub presence_left: bool,
    pub presence_right: bool,
    /// Which cover is attached (from config), if known.
    pub cover: Option<Cover>,
    /// Human-readable cover version string (`"Pod 3"`, `"Pod 4"`, `"unknown"`).
    pub cover_version: String,
    /// Sensor piezo gains `(left, right)` — the cover's `gain*` settings.
    pub gains: (u16, u16),
    /// Hub status-LED brightness (0–100 %), mirroring `config.ron`'s
    /// `led.brightness`. Published by the Frozen manager, which owns the LED.
    pub led_brightness: u8,
}

impl Default for DeviceSnapshot {
    fn default() -> Self {
        DeviceSnapshot {
            left: SideSnapshot::default(),
            right: SideSnapshot::default(),
            water_level: true,
            is_priming: false,
            presence_left: false,
            presence_right: false,
            cover: None,
            cover_version: "unknown".to_string(),
            gains: (0, 0),
            led_brightness: 100,
        }
    }
}

/// An immediate-fire alarm request (the `POST /api/alarm` test-alarm path).
#[derive(Clone, Debug, PartialEq)]
pub struct AlarmSpec {
    pub side: BedSide,
    /// Vibration intensity, 0–100.
    pub intensity: u8,
    pub duration_s: u32,
    pub pattern: AlarmPattern,
}

/// The MQTT broker settings as a *reader* (the api layer, and through it the
/// UI) may see them: deliberately password-free. The stored secret never
/// leaves podd-core — only whether one is set (issue #18).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MqttSnapshot {
    pub enabled: bool,
    pub server: String,
    pub port: u16,
    pub user: String,
    pub password_set: bool,
}

impl Default for MqttSnapshot {
    fn default() -> Self {
        // Pre-config-load placeholder (the watch is created before the file is
        // read). "Not configured" is the honest thing to show if it is ever
        // observed: no broker, no link.
        MqttSnapshot {
            enabled: false,
            server: String::new(),
            port: 1883,
            user: String::new(),
            password_set: false,
        }
    }
}

impl From<&crate::config::MqttConfig> for MqttSnapshot {
    fn from(cfg: &crate::config::MqttConfig) -> Self {
        MqttSnapshot {
            enabled: cfg.enabled,
            server: cfg.server.clone(),
            port: cfg.port,
            user: cfg.user.clone(),
            password_set: !cfg.password.is_empty(),
        }
    }
}

/// An edit to the MQTT broker settings (the UI's Settings → MQTT section).
///
/// `password: None` means *keep the stored password*, so the UI never has to
/// round-trip the secret to change the port.
#[derive(Clone, PartialEq, Eq)]
pub struct MqttUpdate {
    pub enabled: bool,
    pub server: String,
    pub port: u16,
    pub user: String,
    /// `None` = keep whatever is in the config; `Some("")` = clear it.
    pub password: Option<String>,
}

/// Hand-written so a `{cmd:?}` anywhere (the dispatcher logs unmapped commands
/// that way) can never print the broker password.
impl std::fmt::Debug for MqttUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MqttUpdate")
            .field("enabled", &self.enabled)
            .field("server", &self.server)
            .field("port", &self.port)
            .field("user", &self.user)
            .field(
                "password",
                &match &self.password {
                    Some(p) if p.is_empty() => "<cleared>",
                    Some(_) => "<redacted>",
                    None => "<unchanged>",
                },
            )
            .finish()
    }
}

/// A command from a consumer (`api`, schedulers) into the managers. The
/// dispatcher in [`crate::run`] routes each variant to the owning manager.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Set a side's target temperature, integer °F.
    SetTargetTempF { side: BedSide, f: i32 },
    /// Turn a side on/off. `duration_s` is the session length hint (on).
    SetPower { side: BedSide, on: bool, duration_s: u32 },
    /// Clear (dismiss) a vibrating alarm on a side.
    ClearAlarm { side: BedSide },
    /// Prime the water circuit now (always runs; not gated by the daily
    /// "Prime daily?" flag).
    Prime,
    /// Update the *daily* prime schedule in the live config: the UI's
    /// "Prime daily?" toggle + its time. Applied to the config watch and
    /// persisted to `config.ron`, exactly like the MQTT `set_prime` action.
    SetPrimeDaily { enabled: bool, time: Time },
    /// Update per-side away mode in the live config (the UI settings page's
    /// away switches). Applied to the config watch and persisted, like
    /// [`Command::SetPrimeDaily`].
    SetAwayMode { left: bool, right: bool },
    /// Update the schedule timezone in the live config. `iana` is validated
    /// by the API layer; an unknown name is dropped here with an error log.
    SetTimezone { iana: String },
    /// Update the MQTT broker settings in the live config (the UI's
    /// Settings → MQTT section, #18). Applied to the config watch and
    /// persisted like [`Command::SetPrimeDaily`]; the broker *connection*
    /// itself is only rebuilt on the next podd restart.
    SetMqtt(MqttUpdate),
    /// Replace the live per-weekday heating schedule (the UI's Schedule page).
    ///
    /// The API layer has already persisted `schedules.json` — podd-core never
    /// writes that file — so this only refreshes the in-memory schedule the
    /// Frozen manager resolves its target from, exactly like a config-watch
    /// update (manual overrides are dropped). Boxed: [`schedule::Schedules`]
    /// is by far the largest variant and would otherwise bloat every command.
    ///
    ///
    /// [`schedule::Schedules`]: crate::schedule::Schedules
    SetSchedules(Box<crate::schedule::Schedules>),
    /// Replace the live user-settings document (the UI's Settings page).
    ///
    /// Same ownership rule as [`Command::SetSchedules`]: the API layer has
    /// already persisted `settings.json` (podd-core never writes it), so this
    /// only refreshes the in-memory copy the daemon-side consumers watch —
    /// today the daily-reboot scheduler, next the schedule overrides (#106).
    /// Prime/away/timezone are *additionally* bridged into `config.ron` by
    /// their own commands above; this document is not the source of truth for
    /// those.
    SetSettings(Box<crate::settings::Settings>),
    /// Fire an alarm immediately.
    FireAlarm(AlarmSpec),
    /// Set the hub status-LED brightness, 0–100 % (the UI's Settings slider,
    /// #10). Applied to `config.ron`'s `led.brightness` and persisted, like
    /// [`Command::SetPrimeDaily`]; the Frozen manager rescales its patterns
    /// off the config watch.
    SetLedBrightness(u8),
    /// Apply an opaque CBOR device-settings block (gains / LED brightness).
    SetSettingsCbor(Vec<u8>),
    /// Reboot the device.
    Reboot,
    /// Trigger a firmware/software update.
    Update,
    /// Low-level command escape hatch.
    Execute { command: String, arg: Option<String> },
}

/// The consumer-facing halves of the bus. Constructed by [`crate::start`] and
/// handed to `api` (and any other subscriber).
#[derive(Clone)]
pub struct Shared {
    /// Latest device telemetry.
    pub status: watch::Receiver<DeviceSnapshot>,
    /// Latest per-subsystem health (read-only; see [`crate::health`]).
    pub health: watch::Receiver<crate::health::HealthMap>,
    /// Latest MQTT broker settings, password-free ([`MqttSnapshot`]). Mirrors
    /// the live config watch so the api layer can render Settings → MQTT
    /// without ever holding the credential (#18).
    pub mqtt: watch::Receiver<MqttSnapshot>,
    /// Command sink into the managers.
    pub commands: mpsc::Sender<Command>,
    /// Biometrics history (vitals, sleep records, movement), fed by the sensor
    /// manager's processors; a store is `None` if its file could not be opened.
    pub biometrics: crate::biometrics::Stores,
}
