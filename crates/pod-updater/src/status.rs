//! The observable status surface for the update agent.
//!
//! A single [`UpdateStatus`] value is published on a [`tokio::sync::watch`] by
//! the [`crate::Updater`]; the `api` crate (or a CLI) can subscribe and surface
//! it at `GET /api/updates` / the UI's update panel without depending on the
//! agent's internals. Everything here is `Serialize` — `camelCase`, matching
//! the rest of podd's JSON API — so it can go straight onto the wire.

use pod_update::ComponentKind;
use serde::Serialize;

/// The version of one installed component (as the device currently believes it).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    pub kind: ComponentKind,
    pub version: String,
}

/// A component the channel offers that differs from what is installed.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableUpdate {
    pub kind: ComponentKind,
    pub name: String,
    pub version: String,
}

impl From<&pod_update::Component> for AvailableUpdate {
    fn from(c: &pod_update::Component) -> Self {
        AvailableUpdate {
            kind: c.kind,
            name: c.name.clone(),
            version: c.version.clone(),
        }
    }
}

/// Latest-value snapshot of the updater, published on a `watch` channel.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub enabled: bool,
    pub channel: String,
    /// `"auto"` or `"manual"`.
    pub mode: String,
    /// What the device currently runs, per tier.
    pub current_versions: Vec<VersionEntry>,
    /// Unix seconds of the last successful/attempted check (`None` = never).
    pub last_check_unix: Option<i64>,
    /// Whether the last check completed without error.
    pub last_check_ok: bool,
    /// Components offered by the channel that differ from installed.
    pub available: Vec<AvailableUpdate>,
    /// Human-readable last error (check or apply), if any.
    pub last_error: Option<String>,
    /// Human-readable summary of the last successful apply/rollback.
    pub last_applied: Option<String>,
}

impl UpdateStatus {
    pub fn new(enabled: bool, channel: String, mode: String) -> Self {
        UpdateStatus {
            enabled,
            channel,
            mode,
            current_versions: Vec::new(),
            last_check_unix: None,
            last_check_ok: false,
            available: Vec::new(),
            last_error: None,
            last_applied: None,
        }
    }
}

/// Current Unix time in seconds (best-effort; 0 before the epoch).
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
