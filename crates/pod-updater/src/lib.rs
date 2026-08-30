//! `pod-updater` — the on-device OTA update **agent** for the Eight Sleep Pod.
//!
//! This is the device half of the update system charter in `REPLACEMENT_PLAN`
//! §9. A CI pipeline (separate) publishes signed releases; this crate makes the
//! device *pull* them safely: it periodically checks a channel, downloads a
//! signed manifest + artifacts, verifies them, and applies them atomically with
//! a health-checked rollback.
//!
//! It reuses the shared core in [`pod_update`] for the security-critical parts —
//! [`pod_update::Manifest`], [`pod_update::SignedManifest`],
//! [`pod_update::verify_release`], and [`pod_update::Manifest::verify_artifact`]
//! — and adds device-side concerns: transport, filesystem release layout,
//! activation, canary health checks, and per-tier apply.
//!
//! **Trust & integrity.** Authenticity is owner-controlled (see
//! [`config::TrustConfig`] → [`pod_update::TrustPolicy`]): the owner may allow
//! unsigned bundles or require a signature from pinned keys. **Integrity
//! (SHA-256) is always enforced**, regardless of trust policy — a
//! corrupt/truncated artifact is always rejected before use.
//!
//! **Tiers** (`REPLACEMENT_PLAN` §9):
//! - **Tier 2 (App)** — the main path. Atomic release-dir swap under
//!   [`release::ReleaseLayout`] with a canary + instant rollback. Fully live.
//! - **Tier 1 (OS)** and **Tier 3 (MCU)** — detection + verification are live,
//!   but the destructive eMMC A/B write (`fw_setenv`) and STM32 `.bbin` flash
//!   are gated behind [`install::OsSlotWriter`] / [`install::McuFlasher`] with a
//!   `dry_run` default and `// TODO(live-cutover)`.
//!
//! The privileged/destructive/networked steps all sit behind traits
//! ([`source::ReleaseSource`], [`install::ReleaseInstaller`],
//! [`install::HealthCheck`], [`install::OsSlotWriter`],
//! [`install::McuFlasher`]), so the agent logic is fully testable unprivileged
//! and offline.

pub mod agent;
pub mod config;
pub mod error;
pub mod install;
pub mod release;
pub mod source;
pub mod status;

pub use agent::{run_from_env, shared, Updater};
pub use config::{
    PubKeySource, ReleaseSourceUrl, ResolvedSource, TrustConfig, UpdateMode, UpdaterConfig,
    UpdaterPaths,
};
pub use error::{Error, Result};
pub use install::{
    DryMcuFlasher, DryOsSlotWriter, FnHealthCheck, HealthCheck, HttpHealthCheck, McuFlasher,
    NoopInstaller, OsSlotWriter, ReleaseInstaller, SlotPlan, SystemInstaller,
};
pub use release::ReleaseLayout;
pub use source::{build_source, HttpSource, LocalDirSource, MemorySource, ReleaseSource};
pub use status::{AvailableUpdate, UpdateStatus, VersionEntry};

#[cfg(test)]
mod tests {
    use super::*;
    use pod_update::digest::sha256_hex;
    use pod_update::manifest::{Artifact, Component};
    use pod_update::sign::{generate_keypair, sign_manifest};
    use pod_update::{ComponentKind, Manifest, SignedManifest, TrustPolicy};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// A unique temp dir for a test, cleaned up by the caller.
    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pod-updater-{tag}-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn now_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    /// Build a one-app-component manifest whose artifact is `bytes`.
    fn app_manifest(version: &str, filename: &str, bytes: &[u8]) -> Manifest {
        let mut m = Manifest::new("stable", 1_700_000_000);
        m.components.push(Component {
            name: "podd".into(),
            kind: ComponentKind::App,
            version: version.into(),
            artifact: Artifact {
                filename: filename.into(),
                sha256: sha256_hex(bytes),
                size: bytes.len() as u64,
            },
            min_app: None,
        });
        m
    }

    fn paths_in(root: &std::path::Path) -> UpdaterPaths {
        UpdaterPaths {
            release_root: root.join("releases"),
            current_link: root.join("current"),
            staging_dir: root.join("staging"),
        }
    }

    /// Build an updater wired to an in-memory source + noop installer + a health
    /// check whose verdict is controlled by `healthy`.
    fn updater_with(
        root: &std::path::Path,
        manifest_json: String,
        artifacts: Vec<(&str, Vec<u8>)>,
        policy: TrustPolicy,
        healthy: Arc<AtomicBool>,
    ) -> Updater {
        let mut src = MemorySource::new(manifest_json);
        for (name, bytes) in artifacts {
            src = src.with_artifact(name, bytes);
        }
        let paths = paths_in(root);
        std::fs::create_dir_all(&paths.staging_dir).unwrap();
        let layout = ReleaseLayout::new(paths.clone());
        Updater::new(
            "stable",
            UpdateMode::Manual,
            policy,
            vec![Box::new(src)],
            paths.staging_dir,
            layout,
            Box::new(NoopInstaller::default()),
            Box::new(FnHealthCheck(move || healthy.load(Ordering::SeqCst))),
            3,
        )
    }

    #[tokio::test]
    async fn signed_release_end_to_end_happy_path() {
        let root = tmp("happy");
        let bytes = b"squashfs-bytes-v1".to_vec();
        let m = app_manifest("0.0.1+aaa", "app-0.0.1.squashfs", &bytes);
        let (sk, vk) = generate_keypair().unwrap();
        let sm = sign_manifest(&m, &sk).unwrap();

        let up = updater_with(
            &root,
            sm.to_json_pretty().unwrap(),
            vec![("app-0.0.1.squashfs", bytes)],
            TrustPolicy::RequireSigned(vec![vk]),
            Arc::new(AtomicBool::new(true)),
        );

        // check() sees the app update; apply() activates it.
        let available = up.check().await.unwrap();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].kind, ComponentKind::App);

        up.apply(ComponentKind::App).await.unwrap();

        // `current` now points at the new version dir.
        let cur = std::fs::read_link(root.join("current")).unwrap();
        assert_eq!(cur.file_name().unwrap().to_str().unwrap(), "0.0.1+aaa");
        assert!(root.join("releases/0.0.1+aaa/app.squashfs").exists());
        // Status reflects the apply and no available updates after re-check.
        assert!(up.status().last_applied.unwrap().contains("0.0.1+aaa"));
        assert!(up.check().await.unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn tampered_artifact_is_rejected_by_digest() {
        let root = tmp("tamper");
        let real = b"the-real-squashfs".to_vec();
        let m = app_manifest("0.0.1", "app.squashfs", &real);
        let (sk, vk) = generate_keypair().unwrap();
        let sm = sign_manifest(&m, &sk).unwrap();

        // Source serves *different* bytes than the manifest digest expects.
        let up = updater_with(
            &root,
            sm.to_json_pretty().unwrap(),
            vec![("app.squashfs", b"tampered-bytes!!".to_vec())],
            TrustPolicy::RequireSigned(vec![vk]),
            Arc::new(AtomicBool::new(true)),
        );

        let err = up.apply(ComponentKind::App).await.unwrap_err();
        assert!(matches!(err, Error::Core(pod_update::Error::DigestMismatch { .. })
            | Error::Core(pod_update::Error::SizeMismatch { .. })));
        // Nothing was activated.
        assert!(std::fs::read_link(root.join("current")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn unsigned_rejected_under_require_signed_accepted_under_allow_unsigned() {
        let root = tmp("policy");
        let bytes = b"x".to_vec();
        let m = app_manifest("0.0.1", "a.squashfs", &bytes);
        let unsigned = SignedManifest::unsigned(m);
        let (_sk, vk) = generate_keypair().unwrap();

        // RequireSigned + unsigned manifest => check fails.
        let up_req = updater_with(
            &root.join("req"),
            unsigned.to_json_pretty().unwrap(),
            vec![("a.squashfs", bytes.clone())],
            TrustPolicy::RequireSigned(vec![vk]),
            Arc::new(AtomicBool::new(true)),
        );
        assert!(up_req.check().await.is_err());

        // AllowUnsigned + same unsigned manifest => check succeeds, apply works.
        let up_allow = updater_with(
            &root.join("allow"),
            unsigned.to_json_pretty().unwrap(),
            vec![("a.squashfs", bytes)],
            TrustPolicy::AllowUnsigned,
            Arc::new(AtomicBool::new(true)),
        );
        assert_eq!(up_allow.check().await.unwrap().len(), 1);
        up_allow.apply(ComponentKind::App).await.unwrap();
        assert!(std::fs::read_link(root.join("allow/current")).is_ok());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn failing_health_check_rolls_back_the_swap() {
        let root = tmp("canary");
        // Install a healthy v1 first.
        let b1 = b"v1".to_vec();
        let sm1 =
            SignedManifest::unsigned(app_manifest("1.0", "app-1.0.squashfs", &b1));
        let up1 = updater_with(
            &root,
            sm1.to_json_pretty().unwrap(),
            vec![("app-1.0.squashfs", b1)],
            TrustPolicy::AllowUnsigned,
            Arc::new(AtomicBool::new(true)),
        );
        up1.apply(ComponentKind::App).await.unwrap();
        assert_eq!(
            std::fs::read_link(root.join("current"))
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            "1.0"
        );

        // Now attempt v2 whose canary fails => must NOT change current, and the
        // discarded release dir must be gone.
        let b2 = b"v2".to_vec();
        let sm2 =
            SignedManifest::unsigned(app_manifest("2.0", "app-2.0.squashfs", &b2));
        let up2 = updater_with(
            &root,
            sm2.to_json_pretty().unwrap(),
            vec![("app-2.0.squashfs", b2)],
            TrustPolicy::AllowUnsigned,
            Arc::new(AtomicBool::new(false)), // canary fails
        );
        let err = up2.apply(ComponentKind::App).await.unwrap_err();
        assert!(matches!(err, Error::HealthCheckFailed { .. }));
        assert_eq!(
            std::fs::read_link(root.join("current"))
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            "1.0",
            "current must still point at v1 after a failed canary"
        );
        assert!(
            !root.join("releases/2.0").exists(),
            "failed release dir must be discarded"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn rollback_restores_the_previous_release() {
        let root = tmp("rollback");
        let paths = paths_in(&root);
        std::fs::create_dir_all(&paths.staging_dir).unwrap();

        // Install v1 then v2, both healthy, through the layout directly.
        let layout = ReleaseLayout::new(paths.clone());
        let installer = NoopInstaller::default();
        let ok = FnHealthCheck(|| true);

        for (ver, data) in [("1.0", b"one".to_vec()), ("2.0", b"two".to_vec())] {
            let m = app_manifest(ver, "app.squashfs", &data);
            let comp = m.component(ComponentKind::App).unwrap().clone();
            let staged = paths.staging_dir.join(format!("app-{ver}.squashfs"));
            std::fs::write(&staged, &data).unwrap();
            layout
                .install_app(&comp, &staged, &installer, &ok, 3)
                .await
                .unwrap();
        }
        assert_eq!(layout.current_app_version().as_deref(), Some("2.0"));

        // Roll back => current points at v1 again.
        let restored = layout.rollback(&installer).unwrap();
        assert_eq!(restored, "1.0");
        assert_eq!(layout.current_app_version().as_deref(), Some("1.0"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn os_and_mcu_apply_are_gated_dry_run() {
        // A manifest carrying an OS + MCU component; dry-run must succeed
        // without touching hardware, live must refuse (not implemented).
        let root = tmp("tiers");
        let paths = paths_in(&root);
        std::fs::create_dir_all(&paths.staging_dir).unwrap();

        let os_bytes = b"os-image".to_vec();
        let mcu_bytes = b"mcu-bbin".to_vec();
        let mut m = Manifest::new("stable", 1);
        m.components.push(Component {
            name: "os-imx8mm".into(),
            kind: ComponentKind::Os,
            version: "os-1".into(),
            artifact: Artifact {
                filename: "os.raucb".into(),
                sha256: sha256_hex(&os_bytes),
                size: os_bytes.len() as u64,
            },
            min_app: None,
        });
        m.components.push(Component {
            name: "frozen".into(),
            kind: ComponentKind::McuFrozen,
            version: "mcu-9".into(),
            artifact: Artifact {
                filename: "frozen.bbin".into(),
                sha256: sha256_hex(&mcu_bytes),
                size: mcu_bytes.len() as u64,
            },
            min_app: None,
        });
        let sm = SignedManifest::unsigned(m);

        let make = |os_dry: bool, mcu_dry: bool| {
            let src = MemorySource::new(sm.to_json_pretty().unwrap())
                .with_artifact("os.raucb", os_bytes.clone())
                .with_artifact("frozen.bbin", mcu_bytes.clone());
            let layout = ReleaseLayout::new(paths.clone());
            Updater::new(
                "stable",
                UpdateMode::Manual,
                TrustPolicy::AllowUnsigned,
                vec![Box::new(src)],
                paths.staging_dir.clone(),
                layout,
                Box::new(NoopInstaller::default()),
                Box::new(FnHealthCheck(|| true)),
                3,
            )
            .with_dry_run(os_dry, mcu_dry)
        };

        // Dry-run: both succeed but must NOT record their versions — nothing
        // was flashed, so the update stays pending for check()/status (#39).
        let dry = make(true, true);
        dry.apply(ComponentKind::Os).await.unwrap();
        dry.apply(ComponentKind::McuFrozen).await.unwrap();
        let layout = ReleaseLayout::new(paths.clone());
        assert_eq!(layout.installed_version(ComponentKind::Os), None);
        assert_eq!(layout.installed_version(ComponentKind::McuFrozen), None);
        assert_eq!(
            dry.check().await.unwrap().len(),
            2,
            "dry-run apply must leave both updates pending"
        );

        // Live: both refuse with the gated error.
        let live = make(false, false);
        assert!(matches!(
            live.apply(ComponentKind::Os).await.unwrap_err(),
            Error::LiveApplyNotImplemented(ComponentKind::Os)
        ));
        assert!(matches!(
            live.apply(ComponentKind::McuFrozen).await.unwrap_err(),
            Error::LiveApplyNotImplemented(ComponentKind::McuFrozen)
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn source_url_shapes_resolve() {
        // GitHub with explicit tag.
        let gh = ReleaseSourceUrl::GitHub {
            owner: "eightsleep".into(),
            repo: "podd".into(),
            tag: Some("v1.2.3".into()),
        };
        match gh.resolve("manifest.json") {
            ResolvedSource::Http {
                manifest_url,
                artifact_base_url,
            } => {
                assert_eq!(
                    manifest_url,
                    "https://github.com/eightsleep/podd/releases/download/v1.2.3/manifest.json"
                );
                assert_eq!(
                    artifact_base_url,
                    "https://github.com/eightsleep/podd/releases/download/v1.2.3"
                );
            }
            _ => panic!("expected http"),
        }

        // GitHub latest.
        let ghl = ReleaseSourceUrl::GitHub {
            owner: "o".into(),
            repo: "r".into(),
            tag: None,
        };
        match ghl.resolve("manifest.json") {
            ResolvedSource::Http { manifest_url, .. } => assert_eq!(
                manifest_url,
                "https://github.com/o/r/releases/latest/download/manifest.json"
            ),
            _ => panic!(),
        }

        // Gitea/Forgejo self-hosted.
        let gt = ReleaseSourceUrl::Gitea {
            base_url: "https://git.example.org".into(),
            owner: "me".into(),
            repo: "pod".into(),
            tag: "stable".into(),
        };
        match gt.resolve("manifest.json") {
            ResolvedSource::Http {
                manifest_url,
                artifact_base_url,
            } => {
                assert_eq!(
                    manifest_url,
                    "https://git.example.org/me/pod/releases/download/stable/manifest.json"
                );
                assert_eq!(
                    artifact_base_url,
                    "https://git.example.org/me/pod/releases/download/stable"
                );
            }
            _ => panic!(),
        }
    }

    #[test]
    fn env_config_parses_github_and_trust() {
        // Explicit env parsing without touching real process env in parallel:
        // exercise the pure parsers via a constructed config would need env;
        // instead validate the source-shape and trust resolution paths.
        let cfg = UpdaterConfig {
            trust: TrustConfig::Unsigned,
            ..UpdaterConfig::default()
        };
        assert!(matches!(cfg.trust.resolve().unwrap(), TrustPolicy::AllowUnsigned));

        let signed = TrustConfig::Signed(vec![]);
        assert!(signed.resolve().is_err(), "empty key list must error");
    }
}
