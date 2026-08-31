//! Trial/commit state for app-tier activation, carried across the restart.
//!
//! `install_app` cannot canary a new release before flipping `current`:
//! nothing runs the new podd until the service restarts, and that restart
//! kills the very process performing the update (the updater runs inside
//! podd). So activation is a two-phase transaction, mirroring the OS tier's
//! U-Boot bootcount/ustate model:
//!
//! 1. The OLD process stages the release, writes a [`TrialState`] next to the
//!    releases, flips `current`, and restarts the service (dying in the
//!    process).
//! 2. The NEW process resolves the trial: [`early_boot_guard`] (sync, first
//!    thing in `main`) counts boot attempts and rolls `current` back if the
//!    new release keeps dying before it can even serve; [`resolve_trial`]
//!    (async, once the process is up) health-checks the process's *own* API
//!    and either commits the release or rolls back and restarts.
//!
//! systemd re-resolves the `current` symlink on every respawn
//! (`ExecStart=.../current/rootfs/podd`, `Restart=always`), so "roll back"
//! is an atomic symlink flip followed by a process exit/restart.
//!
//! A rolled-back version is remembered in a failure marker so `Auto` mode
//! does not re-try the same broken release every poll (each retry would
//! restart podd twice — and every restart opens the sensor MCU's ~60 s
//! ignore-writes window on a live bed). A manual `apply` clears the marker:
//! the operator explicitly asked to try again.

use crate::config::UpdaterPaths;
use crate::error::Result;
use crate::install::{HealthCheck, ReleaseInstaller};
use crate::release::ReleaseLayout;
use crate::status::now_unix;
use pod_update::ComponentKind;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Boot attempts a trial release gets before [`early_boot_guard`] rolls it
/// back (matches the OS tier's `bootlimit=3`).
pub const MAX_TRIAL_BOOTS: u32 = 3;

const TRIAL_FILE: &str = "trial.json";
const FAILED_FILE: &str = "trial-failed.json";

/// A pending app activation awaiting its post-restart canary verdict.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrialState {
    /// Version being tried (== the release dir basename `current` points at).
    pub new_version: String,
    /// Absolute release dir `current` pointed at before the flip
    /// (`None` on a first install — nothing to roll back to).
    pub old_release: Option<PathBuf>,
    /// Boot attempts consumed so far (bumped by [`early_boot_guard`]).
    pub boots: u32,
    pub started_unix: i64,
}

/// Record of the last activation that was rolled back / abandoned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailedTrial {
    pub version: String,
    pub reason: String,
    pub at_unix: i64,
}

fn trial_path(paths: &UpdaterPaths) -> PathBuf {
    paths.release_root.join(TRIAL_FILE)
}

fn failed_path(paths: &UpdaterPaths) -> PathBuf {
    paths.release_root.join(FAILED_FILE)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Atomic write (temp + rename), same pattern as `versions.json`.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The pending trial, if any.
pub fn load(paths: &UpdaterPaths) -> Option<TrialState> {
    read_json(&trial_path(paths))
}

pub(crate) fn save(paths: &UpdaterPaths, state: &TrialState) -> Result<()> {
    write_json(&trial_path(paths), state)
}

pub(crate) fn clear(paths: &UpdaterPaths) {
    let _ = std::fs::remove_file(trial_path(paths));
}

/// The last rolled-back/abandoned activation, if any.
pub fn last_failure(paths: &UpdaterPaths) -> Option<FailedTrial> {
    read_json(&failed_path(paths))
}

/// Forget the failure marker (a manual apply is explicit consent to retry).
pub(crate) fn clear_failure(paths: &UpdaterPaths) {
    let _ = std::fs::remove_file(failed_path(paths));
}

fn record_failure(paths: &UpdaterPaths, version: &str, reason: &str) {
    let failed = FailedTrial {
        version: version.to_string(),
        reason: reason.to_string(),
        at_unix: now_unix(),
    };
    if let Err(e) = write_json(&failed_path(paths), &failed) {
        log::error!("pod-update-agent: failed to record trial failure: {e}");
    }
}

/// Flip `current` back to the trial's old release and discard the new one.
/// Returns `false` when there is no old release to restore (first install).
fn roll_back_links(paths: &UpdaterPaths, state: &TrialState) -> Result<bool> {
    let Some(old) = &state.old_release else {
        return Ok(false);
    };
    crate::release::atomic_symlink(old, &paths.current_link)?;
    let _ = std::fs::remove_dir_all(paths.release_root.join(&state.new_version));
    Ok(true)
}

/// What [`early_boot_guard`] decided this boot.
#[derive(Debug)]
pub enum BootDecision {
    /// No trial pending — boot normally.
    NoTrial,
    /// A trial is pending and this boot attempt was counted. Proceed;
    /// [`resolve_trial`] will commit or roll back once the process is up.
    TrialBoot(TrialState),
    /// The trial exhausted its boot attempts and `current` was rolled back.
    /// The caller must exit so systemd respawns into the restored release.
    RolledBack { failed_version: String },
}

/// Sync boot-attempt counter — call this before anything else in `main`, so
/// a new release that crashes during startup (config parse, early init)
/// still burns a counted attempt and eventually gets rolled back instead of
/// crash-looping forever one symlink away from a good release.
pub fn early_boot_guard(paths: &UpdaterPaths) -> BootDecision {
    let Some(mut state) = load(paths) else {
        return BootDecision::NoTrial;
    };
    state.boots += 1;
    if state.boots <= MAX_TRIAL_BOOTS {
        if let Err(e) = save(paths, &state) {
            log::error!("pod-update-agent: failed to persist trial boot count: {e}");
        }
        log::info!(
            "pod-update-agent: app release {} on trial (boot attempt {}/{})",
            state.new_version,
            state.boots,
            MAX_TRIAL_BOOTS
        );
        return BootDecision::TrialBoot(state);
    }

    // Out of attempts: the new release keeps dying before it can be
    // health-checked.
    let reason = format!("crashed on {MAX_TRIAL_BOOTS} consecutive boot attempts");
    record_failure(paths, &state.new_version, &reason);
    match roll_back_links(paths, &state) {
        Ok(true) => {
            clear(paths);
            log::error!(
                "pod-update-agent: app release {} {reason}; rolled `current` back",
                state.new_version
            );
            BootDecision::RolledBack {
                failed_version: state.new_version,
            }
        }
        Ok(false) => {
            // First install — nothing to restore. Keep running the only
            // release we have and stop counting.
            clear(paths);
            log::error!(
                "pod-update-agent: app release {} {reason}, but there is no previous \
                 release to roll back to; keeping it",
                state.new_version
            );
            BootDecision::NoTrial
        }
        Err(e) => {
            clear(paths);
            log::error!(
                "pod-update-agent: rollback of app release {} failed: {e}; keeping it",
                state.new_version
            );
            BootDecision::NoTrial
        }
    }
}

/// [`early_boot_guard`] with paths taken from the environment (the same
/// `PODD_UPDATER_RELEASE_ROOT`/`_CURRENT`/`_STAGING` vars the agent uses).
/// Reads env only — safe to call before any config file is parsed.
pub fn early_boot_guard_from_env() -> BootDecision {
    early_boot_guard(&crate::config::UpdaterConfig::from_env().paths)
}

/// The verdict [`resolve_trial`] reached.
#[derive(Debug)]
pub enum TrialOutcome {
    /// Canary healthy: the release was committed.
    Committed { version: String },
    /// Canary failed: `current` was flipped back and the service restarted
    /// (on-device the restart tears this process down).
    RolledBack {
        version: String,
        restored: Option<String>,
    },
    /// Canary failed but there was no previous release to restore; the
    /// release was kept and the trial cleared.
    Abandoned { version: String },
}

/// Async half of the trial: run the canary against this (new) process's own
/// API and commit or roll back. Returns `None` when no trial is pending.
pub async fn resolve_trial(
    layout: &ReleaseLayout,
    installer: &dyn ReleaseInstaller,
    health: &dyn HealthCheck,
    keep: usize,
) -> Option<TrialOutcome> {
    let paths = &layout.paths;
    let state = load(paths)?;
    let version = state.new_version.clone();

    if health.healthy().await {
        if let Err(e) = layout.record_version(ComponentKind::App, &version) {
            log::error!("pod-update-agent: failed to record committed version: {e}");
        }
        clear(paths);
        clear_failure(paths);
        let _ = layout.prune(keep);
        log::info!("pod-update-agent: app release {version} committed (canary healthy)");
        return Some(TrialOutcome::Committed { version });
    }

    record_failure(paths, &version, "post-restart canary health check failed");
    let restored = state
        .old_release
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(str::to_string);
    match roll_back_links(paths, &state) {
        Ok(true) => {
            clear(paths);
            log::error!(
                "pod-update-agent: app release {version} failed its canary; rolling back to {} \
                 and restarting",
                restored.as_deref().unwrap_or("?")
            );
            // On-device this restart kills the current process; the restored
            // release comes up with no trial pending.
            if let Err(e) = installer.restart() {
                log::error!("pod-update-agent: restart after rollback failed: {e}");
            }
            Some(TrialOutcome::RolledBack { version, restored })
        }
        Ok(false) => {
            clear(paths);
            log::error!(
                "pod-update-agent: app release {version} failed its canary, but there is no \
                 previous release to roll back to; keeping it"
            );
            Some(TrialOutcome::Abandoned { version })
        }
        Err(e) => {
            clear(paths);
            log::error!("pod-update-agent: rollback of app release {version} failed: {e}; keeping it");
            Some(TrialOutcome::Abandoned { version })
        }
    }
}
