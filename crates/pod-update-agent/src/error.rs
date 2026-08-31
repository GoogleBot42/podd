//! Errors produced by the device-side update agent.

use pod_update::ComponentKind;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// Verification / manifest error from the shared update core.
    #[error("update core: {0}")]
    Core(#[from] pod_update::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// No configured release source produced a usable, verified manifest.
    #[error("no working release source (last error: {0})")]
    NoSource(String),

    /// The manifest verified but is for a different channel than we track.
    #[error("manifest channel mismatch: want {want}, got {got}")]
    ChannelMismatch { want: String, got: String },

    /// The requested component kind is not present in the manifest.
    #[error("component kind {0:?} not present in manifest")]
    ComponentMissing(ComponentKind),

    /// `rollback()` was called but there is no recorded previous release.
    #[error("no previous release to roll back to")]
    NoPreviousRelease,

    /// A release directory referenced by a symlink is missing on disk.
    #[error("release {0} not found on disk")]
    ReleaseMissing(String),

    /// The component declares a `min_app` the installed app does not satisfy
    /// (or the installed app version is unknown — the gate fails closed).
    #[error(
        "component {name} requires app >= {min_app}, installed app is {installed:?}; update the app first"
    )]
    MinAppNotMet {
        name: String,
        min_app: String,
        installed: Option<String>,
    },

    /// The bootloader (Tier 0) is deliberately excluded from auto-updates.
    #[error("bootloader (Tier 0) is never auto-updated; apply manually")]
    BootloaderRefused,

    /// A destructive MCU flash was requested with dry-run off, but the live
    /// cutover path is not implemented yet (the OS tier's live path is
    /// `crate::os_slot::AbSlotWriter`).
    #[error("live apply for {0:?} is not implemented yet // TODO(live-cutover)")]
    LiveApplyNotImplemented(ComponentKind),

    /// An apply was requested while the agent is switched off. Nothing was
    /// downloaded, staged or activated — say so rather than pretending.
    #[error("update agent is disabled (PODD_UPDATER_ENABLED=false); nothing was applied")]
    Disabled,

    /// A channel name the agent refuses to use (empty, over-long, or carrying
    /// characters that have no business in a URL/filename).
    #[error("invalid channel name: {0}")]
    InvalidChannel(String),

    /// Configuration could not be built/resolved.
    #[error("updater config error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, Error>;
