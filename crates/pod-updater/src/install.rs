//! Trait boundaries that isolate the privileged / destructive steps so the
//! agent logic is testable unprivileged and live cutovers stay gated.
//!
//! - [`ReleaseInstaller`] — make a staged app squashfs runnable and restart the
//!   service. The real impl mounts + `systemctl restart`s (needs root); tests
//!   use [`NoopInstaller`].
//! - [`HealthCheck`] — the canary. Real impl polls the local API; tests use
//!   [`FnHealthCheck`].
//! - [`OsSlotWriter`] / [`McuFlasher`] — Tier 1 / Tier 3. The real impls
//!   **refuse** to perform destructive writes: they log a plan under `dry_run`
//!   and error with `// TODO(live-cutover)` when armed, until the live path
//!   lands.

use crate::error::{Error, Result};
use async_trait::async_trait;
use pod_update::{Component, ComponentKind};
use std::path::Path;

// ---------------------------------------------------------------------------
// Tier 2: app release activation
// ---------------------------------------------------------------------------

/// Make a staged app release runnable and (re)start the service.
///
/// `stage` runs *before* the health check and the `current` flip: it mounts (or
/// extracts) the read-only squashfs so the new release can be exercised.
/// `restart` runs *after* a healthy `current` flip.
pub trait ReleaseInstaller: Send + Sync {
    /// Prepare `squashfs` for execution under `release_dir` (mount/extract).
    fn stage(&self, release_dir: &Path, squashfs: &Path) -> Result<()>;
    /// Restart the running service so it picks up the flipped `current` symlink.
    fn restart(&self) -> Result<()>;
}

/// Test / dev installer: records calls, touches nothing privileged.
#[derive(Default)]
pub struct NoopInstaller {
    pub staged: std::sync::Mutex<Vec<std::path::PathBuf>>,
    pub restarts: std::sync::atomic::AtomicUsize,
}

impl ReleaseInstaller for NoopInstaller {
    fn stage(&self, release_dir: &Path, _squashfs: &Path) -> Result<()> {
        self.staged.lock().unwrap().push(release_dir.to_path_buf());
        Ok(())
    }
    fn restart(&self) -> Result<()> {
        self.restarts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// Real installer: mounts the squashfs read-only under `release_dir/rootfs`
/// and restarts the systemd unit. Requires root; used on the device.
pub struct SystemInstaller {
    /// systemd unit to restart after the flip (e.g. `podd.service`).
    pub service: String,
}

impl ReleaseInstaller for SystemInstaller {
    fn stage(&self, release_dir: &Path, squashfs: &Path) -> Result<()> {
        let mnt = release_dir.join("rootfs");
        std::fs::create_dir_all(&mnt)?;
        let status = std::process::Command::new("mount")
            .args(["-t", "squashfs", "-o", "ro,loop"])
            .arg(squashfs)
            .arg(&mnt)
            .status()?;
        if !status.success() {
            return Err(Error::Config(format!(
                "mount of {} failed: {status}",
                squashfs.display()
            )));
        }
        Ok(())
    }

    fn restart(&self) -> Result<()> {
        let status = std::process::Command::new("systemctl")
            .arg("restart")
            .arg(&self.service)
            .status()?;
        if !status.success() {
            return Err(Error::Config(format!(
                "systemctl restart {} failed: {status}",
                self.service
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Canary health check
// ---------------------------------------------------------------------------

/// The post-activation canary: does the new release look healthy?
#[async_trait]
pub trait HealthCheck: Send + Sync {
    async fn healthy(&self) -> bool;
}

/// A closure-backed health check for tests.
pub struct FnHealthCheck<F>(pub F);

#[async_trait]
impl<F> HealthCheck for FnHealthCheck<F>
where
    F: Fn() -> bool + Send + Sync,
{
    async fn healthy(&self) -> bool {
        (self.0)()
    }
}

/// Poll a local URL (the app's own API) until it answers 2xx within a budget.
pub struct HttpHealthCheck {
    pub client: reqwest::Client,
    pub url: String,
    pub timeout: std::time::Duration,
}

#[async_trait]
impl HealthCheck for HttpHealthCheck {
    async fn healthy(&self) -> bool {
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            if let Ok(resp) = self.client.get(&self.url).send().await {
                if resp.status().is_success() {
                    return true;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Tier 1 (OS) — plumbing only; destructive write is gated
// ---------------------------------------------------------------------------

/// The plan a Tier-1 apply *would* execute (for logging/observability).
#[derive(Debug, Clone)]
pub struct SlotPlan {
    pub inactive_slot: String,
    pub env_flip: String,
}

/// Write a verified OS image to the inactive A/B slot and stage the U-Boot env
/// flip (`fw_setenv`). Implementations MUST NOT write when `dry_run` is true.
#[async_trait]
pub trait OsSlotWriter: Send + Sync {
    async fn write_inactive_slot(
        &self,
        component: &Component,
        image: &Path,
        dry_run: bool,
    ) -> Result<SlotPlan>;
}

/// Default OS writer: computes the plan and logs it under dry-run; refuses to
/// perform the destructive eMMC write + `fw_setenv` flip until the live path is
/// implemented.
pub struct DryOsSlotWriter;

#[async_trait]
impl OsSlotWriter for DryOsSlotWriter {
    async fn write_inactive_slot(
        &self,
        component: &Component,
        image: &Path,
        dry_run: bool,
    ) -> Result<SlotPlan> {
        // In the real i.MX layout the active slot comes from `fw_printenv
        // mmcpart`; the inactive one is the other of {rootfs_a, rootfs_b}. We
        // model that here without touching the device.
        let plan = SlotPlan {
            inactive_slot: "rootfs_b (inactive; from fw_printenv mmcpart)".into(),
            env_flip: "fw_setenv mmcpart <inactive>; fw_setenv bootcount 0".into(),
        };
        log::warn!(
            "OS update {} v{}: would write {} to {} then {} // TODO(live-cutover)",
            component.name,
            component.version,
            image.display(),
            plan.inactive_slot,
            plan.env_flip,
        );
        if dry_run {
            Ok(plan)
        } else {
            // Destructive eMMC A/B write + fw_setenv is deliberately unimplemented.
            Err(Error::LiveApplyNotImplemented(ComponentKind::Os))
        }
    }
}

// ---------------------------------------------------------------------------
// Tier 3 (MCU) — plumbing only; destructive flash is gated
// ---------------------------------------------------------------------------

/// Flash a verified STM32 `.bbin` blob. Implementations MUST NOT write when
/// `dry_run` is true.
#[async_trait]
pub trait McuFlasher: Send + Sync {
    async fn flash(&self, component: &Component, blob: &Path, dry_run: bool) -> Result<()>;
}

/// Default MCU flasher: logs the intent under dry-run; refuses the destructive
/// flash until the live path (LSP bootloader write + readback verify) lands.
pub struct DryMcuFlasher;

#[async_trait]
impl McuFlasher for DryMcuFlasher {
    async fn flash(&self, component: &Component, blob: &Path, dry_run: bool) -> Result<()> {
        log::warn!(
            "MCU update {} ({:?}) v{}: would flash {} then verify readback // TODO(live-cutover)",
            component.name,
            component.kind,
            component.version,
            blob.display(),
        );
        if dry_run {
            Ok(())
        } else {
            Err(Error::LiveApplyNotImplemented(component.kind))
        }
    }
}
