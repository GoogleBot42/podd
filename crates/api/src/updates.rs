//! The update-agent seam: what `GET /api/updates` reports and what its action
//! routes may do.
//!
//! `REPLACEMENT_PLAN` §9 asks for an observability surface over the updater
//! ("app/OS/MCU versions + closure hashes + history", plus channel / "check
//! now" / "roll back" controls). [`UpdateOps`] is that surface, narrowed to
//! what `pod-update-agent` actually implements today:
//!
//! - **read** the published [`UpdateStatus`] (channel, mode, installed
//!   versions, last check, available components, last error/apply),
//! - **check now** — poll the configured channel out of band,
//! - **apply** — install the offered Tier-2 (app) release and restart into it
//!   as a canary that must pass its health check or be rolled back
//!   automatically (`pod_update_agent::trial`); or write the offered Tier-1
//!   (OS) image to the inactive A/B slot and arm U-Boot's rollback (only with
//!   an explicit `confirm`, and still subject to every agent-side gate:
//!   signature/digest verification, `PODD_UPDATER_OS_DRY_RUN`, the writer's
//!   eMMC refusal; issue #184),
//! - **set the channel** — switch channels at runtime, persisted by the agent,
//! - **roll back** — flip the Tier-2 app symlink to the previous release.
//!
//! Deliberately *not* here: applying Tier-3 (MCU) or the bootloader — the
//! route names them through [`UpdateTier`] only to answer `501 Not
//! Implemented` honestly instead of pretending. Nothing on this path touches
//! actuation or alarms.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use pod_update_agent::UpdateStatus;

/// An update tier as named on the wire (`{"kind": "app"}`). Mirrors
/// `pod_update::ComponentKind`'s serde names; [`UpdateTier::App`] and
/// [`UpdateTier::Os`] are appliable through the API (see the module docs).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UpdateTier {
    /// Tier 2: podd + UI.
    #[default]
    App,
    /// Tier 1: OS image (A/B slots). Appliable with `confirm: true` (#184).
    Os,
    /// Tier 3: "Frozen" MCU firmware. Not appliable from the API; issue #43.
    McuFrozen,
    /// Tier 3: "Sensor" MCU firmware. Not appliable from the API; issue #43.
    McuSensor,
    /// Tier 0: bootloader. Never updated automatically.
    Bootloader,
}

/// Body of `POST /api/updates/apply`. `{}` means the app tier.
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct ApplyRequest {
    pub kind: UpdateTier,
    /// Required (`true`) for the OS tier: writing a slot is an explicit
    /// opt-in until the OS-OTA hardware pass is done (#184). Ignored for app.
    pub confirm: bool,
}

/// Body of `POST /api/updates/channel`.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChannelRequest {
    pub channel: String,
}

/// Read/act interface onto the on-device update agent. Implemented for
/// [`pod_update_agent::Updater`]; [`MockUpdates`] backs tests and the example
/// server so the routes are exercisable without an updater.
#[async_trait]
pub trait UpdateOps: Send + Sync {
    /// The agent's latest published status.
    fn status(&self) -> UpdateStatus;

    /// Poll the configured release channel now. The result lands in the
    /// published status either way; the `Err` is for the caller's HTTP code.
    async fn check_now(&self) -> anyhow::Result<()>;

    /// Install the channel's Tier-2 (app) release: verify, stage, flip
    /// `current`, and restart podd into it as a canary that commits itself or
    /// is rolled back automatically. On-device the restart means this call's
    /// HTTP response usually never reaches the client.
    ///
    /// Errs — never silently succeeds — when the agent is disabled, no source
    /// is reachable, or verification fails.
    async fn apply_app(&self) -> anyhow::Result<()>;

    /// Write the channel's Tier-1 (OS) image to the inactive A/B slot, read it
    /// back, and arm U-Boot's bootcount rollback — or, under
    /// `PODD_UPDATER_OS_DRY_RUN` (the default), verify the artifact and log the
    /// plan without writing. Never reboots: the new slot is tried on the next
    /// boot. Errs on the same conditions as [`UpdateOps::apply_app`], and when
    /// an armed apply has no A/B slot writer.
    async fn apply_os(&self) -> anyhow::Result<()>;

    /// Switch the followed release channel and persist the choice so it
    /// survives a restart. Applies nothing on its own: the next check is what
    /// consults the new channel.
    fn set_channel(&self, channel: &str) -> anyhow::Result<()>;

    /// Roll the app tier back to the previous release. Returns the restored
    /// version. On-device this restarts `podd`.
    fn rollback(&self) -> anyhow::Result<String>;
}

#[async_trait]
impl UpdateOps for pod_update_agent::Updater {
    fn status(&self) -> UpdateStatus {
        // Installed versions are cheap to re-read and are the one part of the
        // status that is true even when no source is configured or the last
        // check failed — refresh them so the panel never shows a stale build.
        pod_update_agent::Updater::refresh_versions(self);
        pod_update_agent::Updater::status(self)
    }

    async fn check_now(&self) -> anyhow::Result<()> {
        pod_update_agent::Updater::check(self).await?;
        Ok(())
    }

    async fn apply_app(&self) -> anyhow::Result<()> {
        pod_update_agent::Updater::apply(self, pod_update_agent::ComponentKind::App).await?;
        Ok(())
    }

    async fn apply_os(&self) -> anyhow::Result<()> {
        pod_update_agent::Updater::apply(self, pod_update_agent::ComponentKind::Os).await?;
        Ok(())
    }

    fn set_channel(&self, channel: &str) -> anyhow::Result<()> {
        pod_update_agent::Updater::set_channel(self, channel)?;
        Ok(())
    }

    fn rollback(&self) -> anyhow::Result<String> {
        Ok(pod_update_agent::Updater::rollback(self)?)
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
    /// `"check"` / `"apply"` / `"channel"` / `"rollback"` in call order.
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

    /// Make every action fail (to exercise the error responses).
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

    async fn apply_app(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("apply");
        match &self.fail_with {
            Some(msg) => anyhow::bail!("{msg}"),
            None => {
                let mut status = self.status.lock().unwrap();
                status.last_applied = Some("app -> 0.0.2 (trial; canary after restart)".into());
                status.available.clear();
                Ok(())
            }
        }
    }

    async fn apply_os(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("apply_os");
        match &self.fail_with {
            Some(msg) => anyhow::bail!("{msg}"),
            None => {
                let mut status = self.status.lock().unwrap();
                let how = if status.os_dry_run {
                    "dry-run"
                } else {
                    "written to inactive slot + armed; reboot to activate"
                };
                status.last_applied = Some(format!("os -> 0.0.2 ({how})"));
                Ok(())
            }
        }
    }

    fn set_channel(&self, channel: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("channel");
        match &self.fail_with {
            Some(msg) => anyhow::bail!("{msg}"),
            None => {
                let mut status = self.status.lock().unwrap();
                status.channel = channel.to_string();
                status.available.clear();
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
