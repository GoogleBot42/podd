//! Trait boundaries that isolate the privileged / destructive steps so the
//! agent logic is testable unprivileged and live cutovers stay gated.
//!
//! - [`ReleaseInstaller`] — make a staged app squashfs runnable and restart the
//!   service. The real impl mounts + `systemctl restart`s (needs root); tests
//!   use [`NoopInstaller`].
//! - [`HealthCheck`] — the canary. Real impl polls the local API; tests use
//!   [`FnHealthCheck`].
//! - [`OsSlotWriter`] — Tier 1. Live impl: [`crate::os_slot::AbSlotWriter`]
//!   (write-verify-arm on the SD A/B slots); [`DryOsSlotWriter`] is the
//!   fallback where the A/B contract isn't present.
//! - [`McuFlasher`] — Tier 3. Still gated: [`DryMcuFlasher`] logs a plan under
//!   `dry_run` and errors with `// TODO(live-cutover)` when armed, until the
//!   live flash path lands.

use crate::error::{Error, Result};
use async_trait::async_trait;
use pod_update::Component;
use std::path::Path;

// ---------------------------------------------------------------------------
// Tier 2: app release activation
// ---------------------------------------------------------------------------

/// Make a staged app release runnable and (re)start the service.
///
/// `stage` runs *before* the `current` flip: it mounts (or extracts) the
/// read-only squashfs so the new release can be executed. `restart` runs after
/// the flip — on-device it tears down the process performing the update, and
/// the NEW process then canaries itself (see [`crate::trial`]).
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

/// Real installer: **extracts** the squashfs into `release_dir/rootfs` and
/// restarts the systemd unit. Requires root; used on the device.
///
/// Extraction (not a loop mount) is deliberate: a mount does not survive a
/// reboot, so a mounted release would come back as an empty `rootfs/` on the
/// next boot — the service would exec nothing (installer layout) or silently
/// fall back to the OS-baked binary (clean-room image), un-applying the
/// update. Same strategy as `install/podd-install.sh`: `unsquashfs` when
/// available, else mount + copy + umount.
pub struct SystemInstaller {
    /// systemd unit to restart after the flip (e.g. `podd.service`).
    pub service: String,
}

impl ReleaseInstaller for SystemInstaller {
    fn stage(&self, release_dir: &Path, squashfs: &Path) -> Result<()> {
        let rootfs = release_dir.join("rootfs");
        // A leftover tree from an interrupted stage must not survive into the
        // new release.
        if rootfs.exists() {
            std::fs::remove_dir_all(&rootfs)?;
        }

        let unsquash = std::process::Command::new("unsquashfs")
            .arg("-f")
            .arg("-d")
            .arg(&rootfs)
            .arg(squashfs)
            .status();
        match unsquash {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => log::warn!(
                "pod-update-agent: unsquashfs of {} failed ({status}); falling back to mount+copy",
                squashfs.display()
            ),
            Err(e) => log::info!(
                "pod-update-agent: unsquashfs unavailable ({e}); falling back to mount+copy"
            ),
        }
        // A failed unsquashfs may have left a partial tree.
        if rootfs.exists() {
            std::fs::remove_dir_all(&rootfs)?;
        }
        std::fs::create_dir_all(&rootfs)?;

        let mnt = release_dir.join(".stage-mnt");
        std::fs::create_dir_all(&mnt)?;
        let status = std::process::Command::new("mount")
            .args(["-t", "squashfs", "-o", "ro,loop"])
            .arg(squashfs)
            .arg(&mnt)
            .status()?;
        if !status.success() {
            let _ = std::fs::remove_dir(&mnt);
            return Err(Error::Config(format!(
                "cannot unpack {}: unsquashfs failed/absent and mount failed: {status}",
                squashfs.display()
            )));
        }
        let copied = std::process::Command::new("cp")
            .arg("-a")
            .arg(format!("{}/.", mnt.display()))
            .arg(&rootfs)
            .status();
        let umounted = std::process::Command::new("umount").arg(&mnt).status();
        let _ = std::fs::remove_dir(&mnt);
        match copied {
            Ok(status) if status.success() => {}
            other => {
                let _ = std::fs::remove_dir_all(&rootfs);
                return Err(Error::Config(format!(
                    "copy out of mounted {} failed: {other:?}",
                    squashfs.display()
                )));
            }
        }
        if !matches!(umounted, Ok(s) if s.success()) {
            log::warn!(
                "pod-update-agent: umount of staging mount {} failed; continuing (release \
                 already copied out)",
                mnt.display()
            );
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
///
/// Runs in the NEW process after the restart (see [`crate::trial`]) — the
/// default HTTP impl polls this process's own API, so the verdict genuinely
/// reflects the release under trial.
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

/// Fallback OS writer for systems without the A/B contract (dev boxes,
/// non-A/B installs): computes and logs the plan, and refuses an armed apply —
/// the live path is [`crate::os_slot::AbSlotWriter`], selected automatically
/// when `/etc/fw_env.config` + the slot devices exist (see
/// [`crate::config::OsWriterKind`]).
pub struct DryOsSlotWriter;

#[async_trait]
impl OsSlotWriter for DryOsSlotWriter {
    async fn write_inactive_slot(
        &self,
        component: &Component,
        image: &Path,
        dry_run: bool,
    ) -> Result<SlotPlan> {
        // On the real SD layout the active slot comes from `fw_printenv
        // mmcpart` (1=A=p1, 2=B=p2). We model that here without touching
        // anything.
        let plan = SlotPlan {
            inactive_slot: "inactive SD slot (from fw_printenv mmcpart; 1=A, 2=B)".into(),
            env_flip: "fw_setenv batch: mmcpart/next_mmcpart flip + upgrade_available=1 \
                       bootcount=0 ustate=1"
                .into(),
        };
        log::warn!(
            "OS update {} v{}: would write {} to {} then {} (no A/B slot hardware wired)",
            component.name,
            component.version,
            image.display(),
            plan.inactive_slot,
            plan.env_flip,
        );
        if dry_run {
            Ok(plan)
        } else {
            Err(Error::Config(
                "no A/B slot hardware detected (missing /etc/fw_env.config or slot \
                 devices); cannot live-apply an OS update here"
                    .into(),
            ))
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
