//! Persisted + in-memory state for the API.
//!
//! `Settings` and `Schedules` are persisted to JSON files (LowDB parity:
//! load-or-default on start, atomic write on save). `DeviceStatus` and the
//! presence snapshot live only in memory behind `RwLock`s — later they are fed
//! by `podd-core`'s live state bus; for now they are seeded with sane defaults.

use crate::wire::{
    c_to_f, f_to_level, DeviceStatus, MqttSettings, PresenceState, Schedules, Settings,
    SidePresence, SideStatus,
};
use podd_core::bus::{DeviceSnapshot, MqttSnapshot, SideSnapshot};
use podd_core::health::HealthMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::sync::watch;

/// Where `Settings` / `Schedules` are persisted. `None` = in-memory only
/// (used by tests and the API-only mode).
#[derive(Clone, Debug, Default)]
pub struct StoreConfig {
    pub settings_path: Option<PathBuf>,
    pub schedules_path: Option<PathBuf>,
}

/// Central state container shared across all handlers.
pub struct StateStore {
    config: StoreConfig,
    settings: RwLock<Settings>,
    schedules: RwLock<Schedules>,
    status: RwLock<DeviceStatus>,
    presence: RwLock<PresenceState>,
    /// Latest per-subsystem health from podd-core. Empty until the health
    /// updater is attached (API-only mode / tests), which renders as
    /// "not started" rather than as a lie.
    health: RwLock<HealthMap>,
    /// MQTT broker settings (#18). Owned by `config.ron` — podd-core mirrors
    /// them here (password-free) via [`StateStore::spawn_mqtt_updater`], and
    /// edits travel back out over the command bus, never through this file.
    mqtt: RwLock<MqttSettings>,
}

fn load_or_default<T>(path: &Option<PathBuf>) -> T
where
    T: serde::de::DeserializeOwned + Default,
{
    match path {
        Some(p) if p.exists() => match std::fs::read(p) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("failed to parse {}: {e}; using default", p.display());
                T::default()
            }),
            Err(e) => {
                log::warn!("failed to read {}: {e}; using default", p.display());
                T::default()
            }
        },
        _ => T::default(),
    }
}

/// Atomic JSON write: write to a temp sibling then rename over the target.
fn atomic_write<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_vec_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl StateStore {
    /// Load persisted `Settings`/`Schedules` (or defaults) and seed in-memory
    /// device status + presence with defaults.
    pub fn new(config: StoreConfig) -> Self {
        let settings: Settings = load_or_default(&config.settings_path);
        let schedules: Schedules = load_or_default(&config.schedules_path);
        StateStore {
            config,
            settings: RwLock::new(settings),
            schedules: RwLock::new(schedules),
            status: RwLock::new(DeviceStatus::default()),
            presence: RwLock::new(PresenceState::default()),
            health: RwLock::new(HealthMap::new()),
            mqtt: RwLock::new(MqttSettings::default()),
        }
    }

    /// A fully in-memory store (no persistence). Handy for tests/examples.
    pub fn in_memory() -> Self {
        Self::new(StoreConfig::default())
    }

    /// Build a store whose device-status + presence are driven live by
    /// podd-core's state watch, while `Settings`/`Schedules` stay file-backed
    /// via `config`.
    ///
    /// Seeds from the current snapshot, then spawns a task that re-applies every
    /// subsequent update. Returns an `Arc` (the updater task holds a clone).
    pub fn from_watch(mut rx: watch::Receiver<DeviceSnapshot>, config: StoreConfig) -> Arc<Self> {
        let store = Arc::new(Self::new(config));
        store.apply_snapshot(&rx.borrow_and_update());

        let updater = store.clone();
        tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let snap = rx.borrow_and_update().clone();
                updater.apply_snapshot(&snap);
            }
            log::info!("device-status watch closed; updater exiting");
        });

        store
    }

    /// Mirror podd-core's health watch into the store, so `GET /serverStatus`
    /// is a plain read. Same shape as [`Self::from_watch`]'s updater; separate
    /// because the two watches have independent lifetimes (an API-only build
    /// simply never attaches this one).
    pub fn spawn_health_updater(self: &Arc<Self>, mut rx: watch::Receiver<HealthMap>) {
        self.set_health(rx.borrow_and_update().clone());

        let updater = self.clone();
        tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let health = rx.borrow_and_update().clone();
                updater.set_health(health);
            }
            log::info!("health watch closed; updater exiting");
        });
    }

    /// Mirror podd-core's MQTT-settings watch into the store, so `GET
    /// /api/mqtt` is a plain read (#18). Same shape as
    /// [`Self::spawn_health_updater`]; an API-only build never attaches it and
    /// reports the "not configured" default.
    pub fn spawn_mqtt_updater(self: &Arc<Self>, mut rx: watch::Receiver<MqttSnapshot>) {
        self.set_mqtt(rx.borrow_and_update().clone().into());

        let updater = self.clone();
        tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let snap = rx.borrow_and_update().clone();
                updater.set_mqtt(snap.into());
            }
            log::info!("mqtt-settings watch closed; updater exiting");
        });
    }

    // ---- mqtt broker settings (in-memory; config.ron is the owner) ----

    pub fn mqtt(&self) -> MqttSettings {
        self.mqtt.read().unwrap().clone()
    }

    /// Replace the cached broker settings. Called by the watch updater, and by
    /// the POST handler so a read straight after a write doesn't race the
    /// command bus round-trip.
    pub fn set_mqtt(&self, mqtt: MqttSettings) {
        *self.mqtt.write().unwrap() = mqtt;
    }

    // ---- subsystem health (in-memory only) ----

    pub fn health(&self) -> HealthMap {
        self.health.read().unwrap().clone()
    }

    pub fn set_health(&self, health: HealthMap) {
        *self.health.write().unwrap() = health;
    }

    /// Fold a live [`DeviceSnapshot`] into the in-memory device status + presence.
    pub fn apply_snapshot(&self, snap: &DeviceSnapshot) {
        // Device status: rebuild from the snapshot, preserving the fields
        // podd-core doesn't own (free_sleep/version, wifi, hub, taps defaults).
        {
            let mut status = self.status.write().unwrap();
            let mut next = DeviceStatus {
                left: side_from_snapshot(&snap.left, &status.left),
                right: side_from_snapshot(&snap.right, &status.right),
                water_level: if snap.water_level { "true" } else { "false" }.to_string(),
                is_priming: snap.is_priming,
                cover_version: snap.cover_version.clone(),
                ..status.clone()
            };
            next.settings.gain_left = snap.gains.0 as f64;
            next.settings.gain_right = snap.gains.1 as f64;
            next.settings.led_brightness = snap.led_brightness as i64;
            *status = next;
        }

        // Presence: only bump `lastUpdatedAt` when a side's presence flips.
        {
            let mut presence = self.presence.write().unwrap();
            let now = jiff::Timestamp::now().to_string();
            apply_presence_side(&mut presence.left, snap.presence_left, &now);
            apply_presence_side(&mut presence.right, snap.presence_right, &now);
        }
    }

    // ---- settings ----

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    pub fn set_settings(&self, settings: Settings) -> anyhow::Result<()> {
        if let Some(path) = &self.config.settings_path {
            atomic_write(path, &settings)?;
        }
        *self.settings.write().unwrap() = settings;
        Ok(())
    }

    // ---- schedules ----

    pub fn schedules(&self) -> Schedules {
        self.schedules.read().unwrap().clone()
    }

    pub fn set_schedules(&self, schedules: Schedules) -> anyhow::Result<()> {
        if let Some(path) = &self.config.schedules_path {
            atomic_write(path, &schedules)?;
        }
        *self.schedules.write().unwrap() = schedules;
        Ok(())
    }

    // ---- device status (in-memory only) ----

    pub fn device_status(&self) -> DeviceStatus {
        self.status.read().unwrap().clone()
    }

    /// Replace the in-memory device status snapshot (later called by podd-core).
    pub fn set_device_status(&self, status: DeviceStatus) {
        *self.status.write().unwrap() = status;
    }

    /// Set the hub board identity ("Pod 3", "Pod 4", ...). Host fact — owned by
    /// the podd binary's host-info task, preserved across `apply_snapshot`.
    pub fn set_hub_version(&self, hub_version: String) {
        self.status.write().unwrap().hub_version = hub_version;
    }

    /// Set the WiFi signal strength as a 0–100 percentage (0 = unknown/hidden).
    /// Host fact — owned by the podd binary's host-info task.
    pub fn set_wifi_strength(&self, percent: i32) {
        self.status.write().unwrap().wifi_strength = percent;
    }

    // ---- presence (in-memory only) ----

    pub fn presence(&self) -> PresenceState {
        self.presence.read().unwrap().clone()
    }

    pub fn with_presence_mut<R>(&self, f: impl FnOnce(&mut PresenceState) -> R) -> R {
        let mut guard = self.presence.write().unwrap();
        f(&mut guard)
    }
}

/// Map a bus [`SideSnapshot`] (°C) onto the wire [`SideStatus`] (°F + level),
/// falling back to the previous side's values for anything the snapshot leaves
/// unknown (`None` current/target temps).
fn side_from_snapshot(side: &SideSnapshot, prev: &SideStatus) -> SideStatus {
    let current_f = side.current_temp_c.map(c_to_f);
    let target_f = side.target_temp_c.map(|c| c_to_f(c).round() as i32);
    SideStatus {
        current_temperature_f: current_f.unwrap_or(prev.current_temperature_f),
        current_temperature_level: f_to_level(
            current_f.unwrap_or(prev.current_temperature_f),
        ),
        target_temperature_f: target_f.unwrap_or(prev.target_temperature_f),
        seconds_remaining: side.seconds_remaining,
        is_on: side.is_on,
        is_alarm_vibrating: side.is_alarm_vibrating,
        taps: prev.taps.clone(),
    }
}

fn apply_presence_side(side: &mut SidePresence, present: bool, now: &str) {
    if side.present != present {
        side.present = present;
        side.last_updated_at = now.to_string();
    }
}
