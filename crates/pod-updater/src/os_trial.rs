//! OS-tier trial resolution: the Linux half of the U-Boot A/B state machine.
//!
//! [`crate::os_slot::AbSlotWriter`] writes the inactive slot and arms the env
//! (`upgrade_available=1 bootcount=0 ustate=1` + the `mmcpart` flip). U-Boot
//! then owns the boot-attempt counting and auto-revert (see the state-machine
//! block in `os/board/eightsleep/imx8mm-varsom/uboot-env.txt`). This module is
//! the **mark-good**: once podd is up and healthy after a reboot, it disarms
//! the env (`upgrade_available=0 bootcount=0 ustate=0`) so the slot is
//! committed — and it detects the opposite case, where U-Boot reverted to the
//! old slot.
//!
//! A pending marker (`os-pending.json`, on the persistent /data partition so
//! it survives the slot swap) records which version/slot is on trial; the
//! *booted* slot is read from `/proc/cmdline`, not the env, because after a
//! U-Boot revert the env's `mmcpart` again names the old slot and after a
//! normal armed boot it names the new one — the kernel cmdline is what
//! actually happened this boot.
//!
//! Degradation: with no readable boot env (dev box, non-A/B install) every
//! entry point is a quiet no-op. With an armed env but **no marker** (e.g.
//! `podd-slot-install.sh` armed it), a healthy podd still disarms — podd is
//! the universal `--confirm-good` agent — it just records no version.

use crate::bootenv::BootEnv;
use crate::error::Result;
use crate::install::HealthCheck;
use crate::release::ReleaseLayout;
use pod_update::ComponentKind;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const PENDING_FILE: &str = "os-pending.json";

/// The OS version/slot written and armed, awaiting its first healthy boot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OsPending {
    pub version: String,
    /// Slot the new OS was written to (1=A, 2=B).
    pub slot: u8,
}

fn pending_path(dir: &Path) -> PathBuf {
    dir.join(PENDING_FILE)
}

pub(crate) fn write_pending(dir: &Path, pending: &OsPending) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = pending_path(dir).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(pending)?)?;
    std::fs::rename(&tmp, pending_path(dir))?;
    Ok(())
}

pub fn load_pending(dir: &Path) -> Option<OsPending> {
    let bytes = std::fs::read(pending_path(dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn clear_pending(dir: &Path) {
    let _ = std::fs::remove_file(pending_path(dir));
}

/// Which slot this kernel actually booted from, parsed out of
/// `root=/dev/mmcblk1pN` in the cmdline.
fn slot_from_cmdline(cmdline: &str) -> Option<u8> {
    let root = cmdline
        .split_whitespace()
        .find_map(|arg| arg.strip_prefix("root="))?;
    match root {
        "/dev/mmcblk1p1" => Some(1),
        "/dev/mmcblk1p2" => Some(2),
        _ => None,
    }
}

fn booted_slot() -> Option<u8> {
    let cmdline = std::fs::read_to_string("/proc/cmdline").ok()?;
    slot_from_cmdline(&cmdline)
}

/// What [`resolve_os_trial`] decided.
#[derive(Debug, PartialEq, Eq)]
pub enum OsTrialOutcome {
    /// Healthy boot of the new slot: env disarmed, version recorded.
    Committed { version: String },
    /// The env was armed (by something other than us — no marker) and podd is
    /// healthy: disarmed, nothing recorded.
    Disarmed,
    /// U-Boot reverted to the old slot; the pending version never took.
    RolledBack { version: String },
    /// Armed but podd is not healthy yet — left armed (U-Boot keeps
    /// guarding); the caller should retry later.
    StillArmed,
    /// Armed with `bootcount=0`: the activation reboot has not happened yet.
    /// Nothing to resolve — the arm must survive until the reboot.
    AwaitingReboot,
}

/// Resolve any pending OS trial. Returns `None` when there is nothing to do
/// (env unreadable, or disarmed with no marker).
pub async fn resolve_os_trial(
    layout: &ReleaseLayout,
    env: &dyn BootEnv,
    health: &dyn HealthCheck,
) -> Option<OsTrialOutcome> {
    resolve_with(layout, env, health, booted_slot()).await
}

/// [`resolve_os_trial`] with the booted slot injected (tests aren't running
/// on a kernel with an mmcblk1 root).
async fn resolve_with(
    layout: &ReleaseLayout,
    env: &dyn BootEnv,
    health: &dyn HealthCheck,
    booted: Option<u8>,
) -> Option<OsTrialOutcome> {
    let marker_dir = &layout.paths.release_root;
    let armed = match env.get("upgrade_available") {
        Ok(v) => v.as_deref() == Some("1"),
        Err(e) => {
            log::debug!("pod-updater: boot env unreadable ({e}); no OS trial handling");
            return None;
        }
    };
    let pending = load_pending(marker_dir);

    if armed {
        // `bootcount` distinguishes "armed and rebooted into the trial" from
        // "armed, still running the old OS": the writer arms with bootcount=0
        // and U-Boot's ab_tick increments it on every armed boot. Before the
        // activation reboot nothing may be disarmed — podd's health right now
        // reflects the OLD slot, not the one on trial.
        let boots = match env.get("bootcount") {
            Ok(v) => v.unwrap_or_else(|| "0".into()),
            Err(_) => "0".into(),
        };
        if boots == "0" {
            log::debug!("pod-updater: OS trial armed, awaiting activation reboot");
            return Some(OsTrialOutcome::AwaitingReboot);
        }
        if !health.healthy().await {
            log::warn!(
                "pod-updater: OS trial armed but health check failed; leaving armed \
                 (U-Boot keeps guarding reboots) — will retry"
            );
            return Some(OsTrialOutcome::StillArmed);
        }
        // Disarm FIRST: if we die right after, the next resolution finds a
        // disarmed env + marker + matching slot and completes the commit.
        if let Err(e) = env.set_batch(&[
            ("upgrade_available", "0"),
            ("bootcount", "0"),
            ("ustate", "0"),
        ]) {
            log::error!("pod-updater: failed to disarm OS trial env: {e}; will retry");
            return Some(OsTrialOutcome::StillArmed);
        }
        match pending {
            Some(p) if booted == Some(p.slot) => {
                if let Err(e) = layout.record_version(ComponentKind::Os, &p.version) {
                    log::error!("pod-updater: failed to record OS version: {e}");
                }
                clear_pending(marker_dir);
                log::info!(
                    "pod-updater: OS {} committed on slot {} (healthy boot; env disarmed)",
                    p.version,
                    p.slot
                );
                Some(OsTrialOutcome::Committed { version: p.version })
            }
            Some(p) => {
                // Armed, healthy, but we're NOT on the pending slot — the arm
                // never got its reboot, or something re-armed the old slot.
                // The disarm above cancelled the trial; the write is still on
                // the inactive slot but will not be booted. Surface it.
                clear_pending(marker_dir);
                log::warn!(
                    "pod-updater: OS trial for {} (slot {}) disarmed while still running \
                     slot {:?}; the staged OS will NOT activate — re-apply to retry",
                    p.version,
                    p.slot,
                    booted
                );
                Some(OsTrialOutcome::RolledBack { version: p.version })
            }
            None => {
                log::info!(
                    "pod-updater: boot env was armed with no pending OS record \
                     (external arm, e.g. podd-slot-install.sh); disarmed after healthy boot"
                );
                Some(OsTrialOutcome::Disarmed)
            }
        }
    } else {
        match pending {
            Some(p) if booted == Some(p.slot) => {
                // Disarmed + on the new slot: a previous resolution disarmed
                // but died before recording. Finish the commit.
                if let Err(e) = layout.record_version(ComponentKind::Os, &p.version) {
                    log::error!("pod-updater: failed to record OS version: {e}");
                }
                clear_pending(marker_dir);
                Some(OsTrialOutcome::Committed { version: p.version })
            }
            Some(p) => {
                // Disarmed + on the old slot: U-Boot exhausted the boot
                // attempts and auto-reverted (ustate=3).
                clear_pending(marker_dir);
                log::error!(
                    "pod-updater: OS {} failed its boot trial on slot {}; U-Boot rolled \
                     back to slot {:?}",
                    p.version,
                    p.slot,
                    booted
                );
                Some(OsTrialOutcome::RolledBack { version: p.version })
            }
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootenv::{FakeEnv, UnreadableEnv};
    use crate::config::UpdaterPaths;
    use crate::install::FnHealthCheck;

    fn layout(tag: &str) -> ReleaseLayout {
        let root = std::env::temp_dir().join(format!(
            "podd-os-trial-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        ReleaseLayout::new(UpdaterPaths {
            release_root: root.join("releases"),
            current_link: root.join("current"),
            staging_dir: root.join("staging"),
        })
    }

    fn pending(layout: &ReleaseLayout, version: &str, slot: u8) {
        write_pending(
            &layout.paths.release_root,
            &OsPending {
                version: version.into(),
                slot,
            },
        )
        .unwrap();
    }

    const HEALTHY: FnHealthCheck<fn() -> bool> = FnHealthCheck(|| true);
    const UNHEALTHY: FnHealthCheck<fn() -> bool> = FnHealthCheck(|| false);

    #[tokio::test]
    async fn unreadable_env_is_a_quiet_noop() {
        let l = layout("noenv");
        pending(&l, "1.0.0", 2); // even with a marker present
        let out = resolve_with(&l, &UnreadableEnv, &HEALTHY, Some(2)).await;
        assert!(out.is_none());
        // Marker untouched — a dev box must not eat device state.
        assert!(load_pending(&l.paths.release_root).is_some());
    }

    #[tokio::test]
    async fn fresh_image_is_a_noop() {
        let l = layout("fresh");
        let env = FakeEnv::with(&[("upgrade_available", "0")]);
        let out = resolve_with(&l, &env, &HEALTHY, Some(1)).await;
        assert!(out.is_none());
        assert!(env.batches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn armed_before_reboot_is_left_alone() {
        let l = layout("preboot");
        pending(&l, "1.0.0", 2);
        // Armed with bootcount=0: the activation reboot hasn't happened.
        let env = FakeEnv::with(&[("upgrade_available", "1"), ("bootcount", "0")]);
        let out = resolve_with(&l, &env, &HEALTHY, Some(1)).await;
        assert_eq!(out, Some(OsTrialOutcome::AwaitingReboot));
        assert!(env.batches.lock().unwrap().is_empty(), "must not disarm");
        assert!(load_pending(&l.paths.release_root).is_some());
    }

    #[tokio::test]
    async fn healthy_boot_of_new_slot_commits() {
        let l = layout("commit");
        pending(&l, "2.0.0", 2);
        let env = FakeEnv::with(&[("upgrade_available", "1"), ("bootcount", "1")]);
        let out = resolve_with(&l, &env, &HEALTHY, Some(2)).await;
        assert_eq!(
            out,
            Some(OsTrialOutcome::Committed {
                version: "2.0.0".into()
            })
        );
        // One disarm batch...
        let batches = env.batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        let batch: std::collections::HashMap<_, _> = batches[0].iter().cloned().collect();
        assert_eq!(batch["upgrade_available"], "0");
        assert_eq!(batch["bootcount"], "0");
        assert_eq!(batch["ustate"], "0");
        // ...version recorded, marker gone.
        assert_eq!(
            l.installed_version(ComponentKind::Os).as_deref(),
            Some("2.0.0")
        );
        assert!(load_pending(&l.paths.release_root).is_none());
    }

    #[tokio::test]
    async fn unhealthy_stays_armed() {
        let l = layout("unhealthy");
        pending(&l, "2.0.0", 2);
        let env = FakeEnv::with(&[("upgrade_available", "1"), ("bootcount", "1")]);
        let out = resolve_with(&l, &env, &UNHEALTHY, Some(2)).await;
        assert_eq!(out, Some(OsTrialOutcome::StillArmed));
        assert!(env.batches.lock().unwrap().is_empty(), "must not disarm");
        assert!(load_pending(&l.paths.release_root).is_some());
        assert!(l.installed_version(ComponentKind::Os).is_none());
    }

    #[tokio::test]
    async fn uboot_revert_is_detected() {
        let l = layout("revert");
        pending(&l, "2.0.0", 2);
        // U-Boot reverted: disarmed, ustate=3, and we're back on slot 1.
        let env = FakeEnv::with(&[("upgrade_available", "0"), ("ustate", "3")]);
        let out = resolve_with(&l, &env, &HEALTHY, Some(1)).await;
        assert_eq!(
            out,
            Some(OsTrialOutcome::RolledBack {
                version: "2.0.0".into()
            })
        );
        assert!(load_pending(&l.paths.release_root).is_none());
        assert!(l.installed_version(ComponentKind::Os).is_none());
    }

    #[tokio::test]
    async fn external_arm_is_disarmed_after_healthy_boot() {
        let l = layout("external");
        // Armed + rebooted (bootcount=2), but no marker (podd-slot-install.sh).
        let env = FakeEnv::with(&[("upgrade_available", "1"), ("bootcount", "2")]);
        let out = resolve_with(&l, &env, &HEALTHY, Some(1)).await;
        assert_eq!(out, Some(OsTrialOutcome::Disarmed));
        assert_eq!(env.batches.lock().unwrap().len(), 1);
        assert!(l.installed_version(ComponentKind::Os).is_none());
    }

    #[tokio::test]
    async fn interrupted_commit_is_finished() {
        let l = layout("resume");
        pending(&l, "2.0.0", 2);
        // A previous resolution disarmed but died before recording: disarmed
        // env, marker present, running the new slot.
        let env = FakeEnv::with(&[("upgrade_available", "0")]);
        let out = resolve_with(&l, &env, &HEALTHY, Some(2)).await;
        assert_eq!(
            out,
            Some(OsTrialOutcome::Committed {
                version: "2.0.0".into()
            })
        );
        assert_eq!(
            l.installed_version(ComponentKind::Os).as_deref(),
            Some("2.0.0")
        );
        assert!(load_pending(&l.paths.release_root).is_none());
    }

    #[test]
    fn cmdline_slot_parsing() {
        assert_eq!(
            slot_from_cmdline("console=ttymxc3,115200 root=/dev/mmcblk1p1 rootwait rw"),
            Some(1)
        );
        assert_eq!(
            slot_from_cmdline("root=/dev/mmcblk1p2 rootwait"),
            Some(2)
        );
        // eMMC / unknown roots never map to a slot.
        assert_eq!(slot_from_cmdline("root=/dev/mmcblk2p1 rw"), None);
        assert_eq!(slot_from_cmdline("ro quiet"), None);
    }
}
