//! Tier-1 live OS apply: write a verified OS image to the **inactive** A/B SD
//! slot, verify the write by readback, then arm the U-Boot trial state.
//!
//! Integrity chain (no manifest schema change):
//! 1. compressed artifact — manifest SHA-256 (+ optional ed25519), enforced at
//!    download by the agent before this module ever sees the file;
//! 2. decompression — the zstd frame checksum, verified by the decoder;
//! 3. the medium — hash-while-writing, then re-read the written byte range
//!    from the device and compare digests (`.claude/rules/media-writes.md`:
//!    always verify raw writes).
//!
//! Safety: targets are the fixed SD slot devices (`/dev/mmcblk1p1`/`p2`).
//! eMMC (`mmcblk2`) is **never** a write target — enforced structurally by
//! [`AbSlotWriter::assert_safe_target`] on top of the hardcoded paths. The
//! writer never reboots; activation rides the owner's reboot (or the daily
//! maintenance reboot), and the armed U-Boot env auto-reverts a slot that
//! can't boot (see `uboot-env.txt`).

use crate::bootenv::BootEnv;
use crate::error::{Error, Result};
use crate::install::{OsSlotWriter, SlotPlan};
use crate::os_trial;
use async_trait::async_trait;
use pod_update::Component;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// On-device slot block devices, index 0 = slot 1 (A), index 1 = slot 2 (B).
pub const MMC_SLOT_DEVICES: [&str; 2] = ["/dev/mmcblk1p1", "/dev/mmcblk1p2"];

/// The env vars set (in one batch) to arm a freshly written slot.
fn arm_vars(new_slot: u8, old_slot: u8) -> [(String, String); 5] {
    [
        ("mmcpart".into(), new_slot.to_string()),
        ("next_mmcpart".into(), old_slot.to_string()),
        ("upgrade_available".into(), "1".into()),
        ("bootcount".into(), "0".into()),
        ("ustate".into(), "1".into()),
    ]
}

/// Live A/B slot writer for the clean-room SD image.
pub struct AbSlotWriter {
    pub env: Arc<dyn BootEnv>,
    /// Slot block devices; `[0]` = slot 1 (A), `[1]` = slot 2 (B). Tests point
    /// these at temp files.
    pub slot_devices: [PathBuf; 2],
    /// Where the pending-OS marker lives (the release root, on /data — it must
    /// survive the slot swap).
    pub marker_dir: PathBuf,
    /// Mount table to check write targets against (`/proc/mounts`; tests
    /// inject a snapshot file).
    pub mounts_path: PathBuf,
}

impl AbSlotWriter {
    /// The real on-device wiring.
    pub fn mmc(env: Arc<dyn BootEnv>, marker_dir: PathBuf) -> Self {
        AbSlotWriter {
            env,
            slot_devices: MMC_SLOT_DEVICES.map(PathBuf::from),
            marker_dir,
            mounts_path: PathBuf::from("/proc/mounts"),
        }
    }

    /// The currently booted slot per the U-Boot env. Refuses anything but a
    /// literal "1"/"2" — an unexpected value means the env scheme is not the
    /// one we understand, and guessing picks a write target.
    fn active_slot(&self) -> Result<u8> {
        match self.env.get("mmcpart")?.as_deref() {
            Some("1") => Ok(1),
            Some("2") => Ok(2),
            other => Err(Error::Config(format!(
                "cannot determine active slot: mmcpart={other:?} (want \"1\" or \"2\"); \
                 refusing to pick an OS write target"
            ))),
        }
    }

    /// Hard safety checks on a write target. Never a path resolving to eMMC
    /// (`mmcblk2`), never anything currently mounted.
    fn assert_safe_target(&self, dev: &Path) -> Result<()> {
        // Canonicalize so a symlinked path can't smuggle the target elsewhere.
        let real = dev.canonicalize().map_err(|e| {
            Error::Config(format!("slot device {} unavailable: {e}", dev.display()))
        })?;
        let real_str = real.to_string_lossy();
        if real_str.contains("mmcblk2") {
            return Err(Error::Config(format!(
                "REFUSING OS write: {} resolves to eMMC ({real_str}) — eMMC is never a write target",
                dev.display()
            )));
        }
        if let Ok(mounts) = std::fs::read_to_string(&self.mounts_path) {
            for line in mounts.lines() {
                if let Some(mounted_dev) = line.split_whitespace().next() {
                    if mounted_dev == real_str {
                        return Err(Error::Config(format!(
                            "REFUSING OS write: {real_str} is mounted ({line})"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Stream-decompress `image` onto `target`, fsync, then re-read the
    /// written range from the medium and compare digests. Returns
    /// `(sha256_hex, bytes_written)`.
    fn write_and_verify(image: &Path, target: &Path) -> Result<(String, u64)> {
        let mut dev = std::fs::OpenOptions::new().write(true).read(true).open(target)?;
        let capacity = dev.seek(SeekFrom::End(0))?;
        dev.seek(SeekFrom::Start(0))?;

        let mut decoder = zstd::stream::read::Decoder::new(std::fs::File::open(image)?)?;
        let mut hasher = Sha256::new();
        let mut written: u64 = 0;
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = decoder.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if written + n as u64 > capacity {
                return Err(Error::Config(format!(
                    "OS image larger than slot ({capacity} bytes); aborting (env untouched)"
                )));
            }
            dev.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            written += n as u64;
        }
        dev.flush()?;
        dev.sync_all()?;
        let expected = hex::encode(hasher.finalize());

        // Drop the page cache for this file so the readback hits the medium,
        // not what we just wrote into RAM. Best-effort (fails on tmpfs; the
        // bench test is the ground truth on real hardware).
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::io::AsRawFd;
            libc::posix_fadvise(dev.as_raw_fd(), 0, written as i64, libc::POSIX_FADV_DONTNEED);
        }
        drop(dev);

        let mut reread = std::fs::File::open(target)?;
        let mut hasher = Sha256::new();
        let mut remaining = written;
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            let n = reread.read(&mut buf[..want])?;
            if n == 0 {
                return Err(Error::Config(format!(
                    "OS slot readback ended early ({remaining} bytes short)"
                )));
            }
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
        let actual = hex::encode(hasher.finalize());
        if actual != expected {
            return Err(Error::Config(format!(
                "OS slot readback digest mismatch (wrote {expected}, read {actual}); \
                 slot NOT armed — bad medium?"
            )));
        }
        Ok((expected, written))
    }
}

#[async_trait]
impl OsSlotWriter for AbSlotWriter {
    async fn write_inactive_slot(
        &self,
        component: &Component,
        image: &Path,
        dry_run: bool,
    ) -> Result<SlotPlan> {
        let active = self.active_slot()?;
        let inactive: u8 = 3 - active;
        let target = self.slot_devices[(inactive - 1) as usize].clone();
        self.assert_safe_target(&target)?;

        let plan = SlotPlan {
            inactive_slot: format!("slot {inactive} ({})", target.display()),
            env_flip: format!(
                "fw_setenv batch: mmcpart={inactive} next_mmcpart={active} \
                 upgrade_available=1 bootcount=0 ustate=1"
            ),
        };
        if dry_run {
            log::warn!(
                "[dry-run] OS update {} v{}: would stream {} onto {}, verify readback, then {}",
                component.name,
                component.version,
                image.display(),
                plan.inactive_slot,
                plan.env_flip,
            );
            return Ok(plan);
        }

        let image = image.to_path_buf();
        let target_c = target.clone();
        let (digest, written) =
            tokio::task::spawn_blocking(move || Self::write_and_verify(&image, &target_c))
                .await
                .map_err(|e| Error::Config(format!("OS slot write task failed: {e}")))??;

        // Marker before arming: if we die between the two, the marker is
        // stale-but-harmless (resolution sees upgrade_available unset + the
        // old slot booted, and just clears it).
        os_trial::write_pending(
            &self.marker_dir,
            &os_trial::OsPending {
                version: component.version.clone(),
                slot: inactive,
            },
        )?;
        let vars = arm_vars(inactive, active);
        let vars_ref: Vec<(&str, &str)> =
            vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        if let Err(e) = self.env.set_batch(&vars_ref) {
            // Don't leave a pending marker for an arm that never happened.
            os_trial::clear_pending(&self.marker_dir);
            return Err(e);
        }

        log::warn!(
            "OS update {} v{}: wrote+verified {written} bytes (sha256 {digest}) to {}; \
             armed for trial — REBOOT TO ACTIVATE (the daily maintenance reboot will \
             pick it up; U-Boot auto-reverts if the new slot can't boot)",
            component.name,
            component.version,
            plan.inactive_slot,
        );
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootenv::FakeEnv;
    use pod_update::manifest::{Artifact, Component};
    use pod_update::ComponentKind;
    use std::io::Write as _;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "podd-os-slot-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn component(version: &str) -> Component {
        Component {
            name: "os-image".into(),
            kind: ComponentKind::Os,
            version: version.into(),
            artifact: Artifact {
                filename: format!("os-{version}.ext4.zst"),
                sha256: "unused-here".into(),
                size: 0,
            },
            min_app: None,
        }
    }

    /// A writer over temp files: two 8 MiB "slot devices", an unrelated mounts
    /// snapshot. Returns the still-inspectable env alongside.
    fn writer(dir: &Path, env: FakeEnv) -> (AbSlotWriter, Arc<FakeEnv>, PathBuf, PathBuf) {
        let slot1 = dir.join("slot1.img");
        let slot2 = dir.join("slot2.img");
        for s in [&slot1, &slot2] {
            let f = std::fs::File::create(s).unwrap();
            f.set_len(8 * 1024 * 1024).unwrap();
        }
        let mounts = dir.join("mounts");
        std::fs::write(&mounts, "/dev/mmcblk1p3 /data ext4 rw 0 0\n").unwrap();
        let env = Arc::new(env);
        let w = AbSlotWriter {
            env: env.clone(),
            slot_devices: [slot1.clone(), slot2.clone()],
            marker_dir: dir.join("releases"),
            mounts_path: mounts,
        };
        (w, env, slot1, slot2)
    }

    fn zstd_artifact(dir: &Path, payload: &[u8]) -> PathBuf {
        let path = dir.join("os-test.ext4.zst");
        let f = std::fs::File::create(&path).unwrap();
        let mut enc = zstd::stream::write::Encoder::new(f, 3).unwrap();
        enc.write_all(payload).unwrap();
        enc.finish().unwrap();
        path
    }

    fn fake_env_slot1() -> FakeEnv {
        FakeEnv::with(&[("mmcpart", "1"), ("upgrade_available", "0")])
    }

    #[tokio::test]
    async fn live_write_verifies_and_arms_in_one_batch() {
        let dir = tmp("happy");
        let payload: Vec<u8> = (0..5 * 1024 * 1024u32).map(|i| (i * 31 % 251) as u8).collect();
        let artifact = zstd_artifact(&dir, &payload);
        let (w, env, _slot1, slot2) = writer(&dir, fake_env_slot1());

        w.write_inactive_slot(&component("1.2.3"), &artifact, false)
            .await
            .unwrap();

        // Payload landed byte-exact at the start of the inactive slot (2).
        let written = std::fs::read(&slot2).unwrap();
        assert_eq!(&written[..payload.len()], &payload[..]);

        // Exactly one batched env write carrying the full arm quintet.
        let batches = env.batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        let batch: std::collections::HashMap<_, _> =
            batches[0].iter().cloned().collect();
        assert_eq!(batch["mmcpart"], "2");
        assert_eq!(batch["next_mmcpart"], "1");
        assert_eq!(batch["upgrade_available"], "1");
        assert_eq!(batch["bootcount"], "0");
        assert_eq!(batch["ustate"], "1");

        // Pending marker records version + slot.
        let pending = crate::os_trial::load_pending(&dir.join("releases")).unwrap();
        assert_eq!(pending.version, "1.2.3");
        assert_eq!(pending.slot, 2);
    }

    #[tokio::test]
    async fn dry_run_touches_nothing() {
        let dir = tmp("dry");
        let payload = vec![7u8; 1024];
        let artifact = zstd_artifact(&dir, &payload);
        let env = fake_env_slot1();
        let (w, _env, _s1, slot2) = writer(&dir, env);

        w.write_inactive_slot(&component("1.2.3"), &artifact, true)
            .await
            .unwrap();

        // Slot untouched (still zeros), env untouched, no marker.
        let bytes = std::fs::read(&slot2).unwrap();
        assert!(bytes.iter().all(|&b| b == 0));
        assert!(crate::os_trial::load_pending(&dir.join("releases")).is_none());
    }

    #[tokio::test]
    async fn oversize_image_refused_env_untouched() {
        let dir = tmp("oversize");
        let payload = vec![1u8; 9 * 1024 * 1024]; // > 8 MiB slot
        let artifact = zstd_artifact(&dir, &payload);
        let (w, _env, _s1, _s2) = writer(&dir, fake_env_slot1());

        let err = w
            .write_inactive_slot(&component("1.2.3"), &artifact, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("larger than slot"), "{err}");
        assert!(crate::os_trial::load_pending(&dir.join("releases")).is_none());
    }

    #[tokio::test]
    async fn garbage_mmcpart_refused() {
        let dir = tmp("badpart");
        let payload = vec![1u8; 16];
        let artifact = zstd_artifact(&dir, &payload);
        let (w, _env, _s1, _s2) = writer(&dir, FakeEnv::with(&[("mmcpart", "7")]));

        let err = w
            .write_inactive_slot(&component("1.2.3"), &artifact, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot determine active slot"), "{err}");
    }

    #[tokio::test]
    async fn mounted_target_refused() {
        let dir = tmp("mounted");
        let payload = vec![1u8; 16];
        let artifact = zstd_artifact(&dir, &payload);
        let (mut w, _env, _s1, slot2) = writer(&dir, fake_env_slot1());
        // Mount table lists the inactive slot's (canonical) device.
        let mounts = dir.join("mounts");
        std::fs::write(
            &mounts,
            format!("{} / ext4 rw 0 0\n", slot2.canonicalize().unwrap().display()),
        )
        .unwrap();
        w.mounts_path = mounts;

        let err = w
            .write_inactive_slot(&component("1.2.3"), &artifact, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("is mounted"), "{err}");
    }

    #[test]
    fn emmc_paths_are_refused() {
        // The safety assert rejects anything canonicalizing to mmcblk2, even
        // if someone misconfigures the slot devices. Use a symlink so the
        // path exists.
        let dir = tmp("emmc");
        let real = dir.join("mmcblk2p1");
        std::fs::write(&real, b"").unwrap();
        let link = dir.join("innocent");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (w, _env, _s1, _s2) = writer(&dir, fake_env_slot1());
        let err = w.assert_safe_target(&link).unwrap_err();
        assert!(err.to_string().contains("eMMC"), "{err}");
    }

    #[tokio::test]
    async fn corrupt_artifact_fails_decode() {
        let dir = tmp("corrupt");
        let payload = vec![9u8; 64 * 1024];
        let artifact = zstd_artifact(&dir, &payload);
        // Flip a byte in the compressed body (past the header).
        let mut bytes = std::fs::read(&artifact).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&artifact, &bytes).unwrap();
        let (w, _env, _s1, _s2) = writer(&dir, fake_env_slot1());

        let res = w
            .write_inactive_slot(&component("1.2.3"), &artifact, false)
            .await;
        assert!(res.is_err(), "corrupted zstd stream must not apply");
        assert!(crate::os_trial::load_pending(&dir.join("releases")).is_none());
    }
}
