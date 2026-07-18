//! Persisted + in-memory state for the API.
//!
//! `Settings` and `Schedules` are persisted to JSON files (LowDB parity:
//! load-or-default on start, atomic write on save). `DeviceStatus` and the
//! presence snapshot live only in memory behind `RwLock`s — later they are fed
//! by `podd-core`'s live state bus; for now they are seeded with sane defaults.

use crate::wire::{DeviceStatus, PresenceState, Schedules, Settings};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

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
        }
    }

    /// A fully in-memory store (no persistence). Handy for tests/examples.
    pub fn in_memory() -> Self {
        Self::new(StoreConfig::default())
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

    // ---- presence (in-memory only) ----

    pub fn presence(&self) -> PresenceState {
        self.presence.read().unwrap().clone()
    }

    pub fn with_presence_mut<R>(&self, f: impl FnOnce(&mut PresenceState) -> R) -> R {
        let mut guard = self.presence.write().unwrap();
        f(&mut guard)
    }
}
