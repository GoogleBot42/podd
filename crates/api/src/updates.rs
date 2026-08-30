//! The update-agent seam: what `GET /api/updates` reports and what its two
//! action routes may do.
//!
//! `REPLACEMENT_PLAN` §9 asks for an observability surface over the updater
//! ("app/OS/MCU versions + closure hashes + history", plus channel / "check
//! now" / "roll back" controls). [`UpdateOps`] is that surface, narrowed to
//! what `pod-updater` actually implements today:
//!
//! - **read** the published [`UpdateStatus`] (channel, mode, installed
//!   versions, last check, available components, last error/apply),
//! - **check now** — poll the configured channel out of band,
//! - **roll back** — flip the Tier-2 app symlink to the previous release.
//!
//! Deliberately *not* here: applying an update (that is the other half of
//! issue #1), switching channels at runtime (the channel comes from
//! `PODD_UPDATER_CHANNEL`; there is no runtime setter to expose), and the
//! Tier-1/Tier-3 live apply paths still gated behind their dry-run defaults
//! (issue #43). Nothing on this path touches actuation or alarms.

use async_trait::async_trait;
use serde::Serialize;

pub use pod_updater::UpdateStatus;

/// Read/act interface onto the on-device update agent. Implemented for
/// [`pod_updater::Updater`]; [`MockUpdates`] backs tests and the example
/// server so the routes are exercisable without an updater.
#[async_trait]
pub trait UpdateOps: Send + Sync {
    /// The agent's latest published status.
    fn status(&self) -> UpdateStatus;

    /// Poll the configured release channel now. The result lands in the
    /// published status either way; the `Err` is for the caller's HTTP code.
    async fn check_now(&self) -> anyhow::Result<()>;

    /// Roll the app tier back to the previous release. Returns the restored
    /// version. On-device this restarts `podd`.
    fn rollback(&self) -> anyhow::Result<String>;
}

#[async_trait]
impl UpdateOps for pod_updater::Updater {
    fn status(&self) -> UpdateStatus {
        // Installed versions are cheap to re-read and are the one part of the
        // status that is true even when no source is configured or the last
        // check failed — refresh them so the panel never shows a stale build.
        pod_updater::Updater::refresh_versions(self);
        pod_updater::Updater::status(self)
    }

    async fn check_now(&self) -> anyhow::Result<()> {
        pod_updater::Updater::check(self).await?;
        Ok(())
    }

    fn rollback(&self) -> anyhow::Result<String> {
        Ok(pod_updater::Updater::rollback(self)?)
    }
}

/// Build identity of the running daemon — the same stamp `GET /api/deviceStatus`
/// reports under `freeSleep`, repeated here so the update panel has a version
/// to show on a device that was never installed through the updater (no
/// release dir, so the agent knows no App version).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DaemonBuild {
    /// `git describe` of the running binary.
    pub version: String,
    /// Short commit hash of the running binary.
    pub rev: String,
}

impl Default for DaemonBuild {
    fn default() -> Self {
        DaemonBuild {
            version: podd_core::VERSION.to_string(),
            rev: podd_core::GIT_REV.to_string(),
        }
    }
}

/// `GET /api/updates`: what the device runs, plus the update agent's state.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UpdatesReport {
    pub daemon: DaemonBuild,
    /// `None` when no update agent is wired (API-only mode, or the agent
    /// failed to build) — rendered as "unavailable", never as "up to date".
    pub updater: Option<UpdateStatus>,
}

/// In-memory [`UpdateOps`] returning a canned status and recording actions.
pub struct MockUpdates {
    status: std::sync::Mutex<UpdateStatus>,
    /// `("check", ...)` / `("rollback", ...)` in call order.
    calls: std::sync::Mutex<Vec<&'static str>>,
    /// When set, both actions fail with this message.
    fail_with: Option<String>,
}

impl Default for MockUpdates {
    fn default() -> Self {
        MockUpdates::new(UpdateStatus::new(
            true,
            "stable".to_string(),
            "manual".to_string(),
        ))
    }
}

impl MockUpdates {
    pub fn new(status: UpdateStatus) -> Self {
        MockUpdates {
            status: std::sync::Mutex::new(status),
            calls: std::sync::Mutex::new(Vec::new()),
            fail_with: None,
        }
    }

    /// Make both actions fail (to exercise the error responses).
    pub fn failing(mut self, message: impl Into<String>) -> Self {
        self.fail_with = Some(message.into());
        self
    }

    /// Actions taken against this mock, in order.
    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl UpdateOps for MockUpdates {
    fn status(&self) -> UpdateStatus {
        self.status.lock().unwrap().clone()
    }

    async fn check_now(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("check");
        match &self.fail_with {
            Some(msg) => anyhow::bail!("{msg}"),
            None => {
                self.status.lock().unwrap().last_check_ok = true;
                Ok(())
            }
        }
    }

    fn rollback(&self) -> anyhow::Result<String> {
        self.calls.lock().unwrap().push("rollback");
        match &self.fail_with {
            Some(msg) => anyhow::bail!("{msg}"),
            None => Ok("0.0.1".to_string()),
        }
    }
}
