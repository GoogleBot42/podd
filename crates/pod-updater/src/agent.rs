//! The device-side update agent: check, apply (per tier), roll back, and the
//! background poll loop.
//!
//! Verification order, always:
//! 1. Fetch the signed manifest JSON from a source.
//! 2. `verify_release(&sm, &policy)` — authenticity per the owner's trust policy
//!    (or accept-unsigned). Integrity is enforced regardless in step 4.
//! 3. Match the channel; compute which components differ from installed.
//! 4. For a component being applied: download the artifact to staging and
//!    `verify_artifact` (size+digest) **before** using it. A digest mismatch or
//!    truncation is rejected and the next source (if any) is tried.
//! 5. Dispatch by tier: App = atomic swap + canary; OS/MCU = gated plumbing.

use crate::config::{UpdateMode, UpdaterConfig};
use crate::error::{Error, Result};
use crate::install::{
    DryMcuFlasher, DryOsSlotWriter, HealthCheck, HttpHealthCheck, McuFlasher, OsSlotWriter,
    ReleaseInstaller, SystemInstaller,
};
use crate::release::ReleaseLayout;
use crate::source::{build_source, ReleaseSource};
use crate::status::{now_unix, AvailableUpdate, UpdateStatus, VersionEntry};
use pod_update::{Component, ComponentKind, Manifest, SignedManifest, TrustPolicy};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

/// The agent. Construct with [`Updater::from_config`] (real transports) or the
/// field-wise [`Updater::new`] + `with_*` setters (tests/custom wiring).
pub struct Updater {
    channel: String,
    mode: UpdateMode,
    policy: TrustPolicy,
    sources: Vec<Box<dyn ReleaseSource>>,
    staging_dir: PathBuf,
    layout: ReleaseLayout,
    installer: Box<dyn ReleaseInstaller>,
    health: Box<dyn HealthCheck>,
    os_writer: Box<dyn OsSlotWriter>,
    mcu_flasher: Box<dyn McuFlasher>,
    keep_releases: usize,
    os_dry_run: bool,
    mcu_dry_run: bool,
    enabled: bool,
    poll_interval: std::time::Duration,
    status_tx: watch::Sender<UpdateStatus>,
}

impl Updater {
    /// Assemble an updater from its parts. `os_writer`/`mcu_flasher` default to
    /// the gated dry implementations; override via `with_*`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel: impl Into<String>,
        mode: UpdateMode,
        policy: TrustPolicy,
        sources: Vec<Box<dyn ReleaseSource>>,
        staging_dir: PathBuf,
        layout: ReleaseLayout,
        installer: Box<dyn ReleaseInstaller>,
        health: Box<dyn HealthCheck>,
        keep_releases: usize,
    ) -> Self {
        let channel = channel.into();
        let status = UpdateStatus::new(true, channel.clone(), mode.as_str().into());
        let (status_tx, _) = watch::channel(status);
        Updater {
            channel,
            mode,
            policy,
            sources,
            staging_dir,
            layout,
            installer,
            health,
            os_writer: Box::new(DryOsSlotWriter),
            mcu_flasher: Box::new(DryMcuFlasher),
            keep_releases,
            os_dry_run: true,
            mcu_dry_run: true,
            enabled: true,
            poll_interval: std::time::Duration::from_secs(3600),
            status_tx,
        }
    }

    pub fn with_os_writer(mut self, w: Box<dyn OsSlotWriter>) -> Self {
        self.os_writer = w;
        self
    }
    pub fn with_mcu_flasher(mut self, f: Box<dyn McuFlasher>) -> Self {
        self.mcu_flasher = f;
        self
    }
    pub fn with_dry_run(mut self, os: bool, mcu: bool) -> Self {
        self.os_dry_run = os;
        self.mcu_dry_run = mcu;
        self
    }
    pub fn with_poll_interval(mut self, d: std::time::Duration) -> Self {
        self.poll_interval = d;
        self
    }

    /// A `watch::Receiver` the `api` crate (or a CLI) can subscribe to for the
    /// latest [`UpdateStatus`].
    pub fn status_watch(&self) -> watch::Receiver<UpdateStatus> {
        self.status_tx.subscribe()
    }

    /// A clone of the current status.
    pub fn status(&self) -> UpdateStatus {
        self.status_tx.borrow().clone()
    }

    fn set_status(&self, f: impl FnOnce(&mut UpdateStatus)) {
        self.status_tx.send_modify(f);
    }

    /// Fetch and verify the manifest from the first source that yields a valid,
    /// channel-matching, trust-policy-satisfying manifest.
    async fn fetch_verified(&self) -> Result<Manifest> {
        let mut last_err = String::from("no sources configured");
        for src in &self.sources {
            let json = match src.fetch_manifest().await {
                Ok(j) => j,
                Err(e) => {
                    last_err = format!("{}: {e}", src.label());
                    continue;
                }
            };
            let sm = match SignedManifest::from_json(&json) {
                Ok(sm) => sm,
                Err(e) => {
                    last_err = format!("{}: bad manifest json: {e}", src.label());
                    continue;
                }
            };
            match pod_update::verify_release(&sm, &self.policy) {
                Ok(m) if m.channel == self.channel => return Ok(m),
                Ok(m) => {
                    last_err = format!(
                        "{}: channel mismatch (want {}, got {})",
                        src.label(),
                        self.channel,
                        m.channel
                    );
                }
                Err(e) => {
                    last_err = format!("{}: verification failed: {e}", src.label());
                }
            }
        }
        Err(Error::NoSource(last_err))
    }

    /// Check the channel and report which components differ from installed.
    /// Updates the published status.
    pub async fn check(&self) -> Result<Vec<Component>> {
        let result = self.fetch_verified().await;
        let now = now_unix();
        match result {
            Ok(manifest) => {
                let available: Vec<Component> = manifest
                    .components
                    .iter()
                    .filter(|c| {
                        self.layout.installed_version(c.kind).as_deref() != Some(c.version.as_str())
                    })
                    .cloned()
                    .collect();
                let versions = self.current_versions(&manifest);
                let avail_status: Vec<AvailableUpdate> =
                    available.iter().map(AvailableUpdate::from).collect();
                self.set_status(|s| {
                    s.last_check_unix = Some(now);
                    s.last_check_ok = true;
                    s.current_versions = versions;
                    s.available = avail_status;
                    s.last_error = None;
                });
                Ok(available)
            }
            Err(e) => {
                let msg = e.to_string();
                self.set_status(|s| {
                    s.last_check_unix = Some(now);
                    s.last_check_ok = false;
                    s.last_error = Some(msg);
                });
                Err(e)
            }
        }
    }

    /// Installed versions per tier (App from the symlink; others from the record
    /// or the manifest as a fallback label).
    fn current_versions(&self, _manifest: &Manifest) -> Vec<VersionEntry> {
        [
            ComponentKind::App,
            ComponentKind::Os,
            ComponentKind::McuFrozen,
            ComponentKind::McuSensor,
            ComponentKind::Bootloader,
        ]
        .into_iter()
        .filter_map(|kind| {
            self.layout
                .installed_version(kind)
                .map(|version| VersionEntry { kind, version })
        })
        .collect()
    }

    /// Download a component's artifact to staging and verify size+digest before
    /// returning its path. Tries each source; a tampered/truncated artifact from
    /// one source is rejected and the next is tried.
    async fn download_verified(
        &self,
        manifest: &Manifest,
        component: &Component,
    ) -> Result<PathBuf> {
        let filename = &component.artifact.filename;
        let mut last_err = Error::NoSource("no sources produced a valid artifact".into());
        for src in &self.sources {
            let part = self.staging_dir.join(format!("{filename}.download"));
            if let Err(e) = src.fetch_artifact(filename, &part).await {
                last_err = e;
                continue;
            }
            match manifest.verify_artifact(component, &part) {
                Ok(()) => {
                    let dest = self.staging_dir.join(filename);
                    std::fs::rename(&part, &dest)?;
                    return Ok(dest);
                }
                Err(e) => {
                    // Never trust a digest-mismatched file: discard and try next.
                    let _ = std::fs::remove_file(&part);
                    last_err = Error::Core(e);
                }
            }
        }
        Err(last_err)
    }

    /// Apply the newest manifest's component of `kind`. App = atomic swap +
    /// canary; OS/MCU = verified + gated plumbing; Bootloader is refused.
    pub async fn apply(&self, kind: ComponentKind) -> Result<()> {
        if kind == ComponentKind::Bootloader {
            return Err(Error::BootloaderRefused);
        }
        let manifest = self.fetch_verified().await?;
        let component = manifest
            .component(kind)
            .ok_or(Error::ComponentMissing(kind))?
            .clone();
        let staged = self.download_verified(&manifest, &component).await?;

        let outcome = match kind {
            ComponentKind::App => {
                self.layout
                    .install_app(
                        &component,
                        &staged,
                        &*self.installer,
                        &*self.health,
                        self.keep_releases,
                    )
                    .await
                    .map(|_| format!("app -> {}", component.version))
            }
            ComponentKind::Os => self
                .os_writer
                .write_inactive_slot(&component, &staged, self.os_dry_run)
                .await
                .and_then(|_| {
                    // A dry-run applied nothing: leave the recorded version
                    // alone so check()/status keep reporting the update as
                    // pending (#39).
                    if self.os_dry_run {
                        Ok(())
                    } else {
                        self.layout.record_version(kind, &component.version)
                    }
                })
                .map(|_| {
                    format!(
                        "os -> {} ({})",
                        component.version,
                        if self.os_dry_run { "dry-run" } else { "live" }
                    )
                }),
            ComponentKind::McuFrozen | ComponentKind::McuSensor => self
                .mcu_flasher
                .flash(&component, &staged, self.mcu_dry_run)
                .await
                .and_then(|_| {
                    if self.mcu_dry_run {
                        Ok(())
                    } else {
                        self.layout.record_version(kind, &component.version)
                    }
                })
                .map(|_| {
                    format!(
                        "{:?} -> {} ({})",
                        kind,
                        component.version,
                        if self.mcu_dry_run { "dry-run" } else { "live" }
                    )
                }),
            ComponentKind::Bootloader => unreachable!("refused above"),
        };

        match outcome {
            Ok(summary) => {
                self.set_status(|s| {
                    s.last_applied = Some(summary);
                    s.last_error = None;
                });
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                self.set_status(|s| s.last_error = Some(msg));
                Err(e)
            }
        }
    }

    /// Roll the app tier back to the previous release.
    pub fn rollback(&self) -> Result<String> {
        match self.layout.rollback(&*self.installer) {
            Ok(v) => {
                self.set_status(|s| {
                    s.last_applied = Some(format!("rollback -> {v}"));
                    s.last_error = None;
                });
                Ok(v)
            }
            Err(e) => {
                let msg = e.to_string();
                self.set_status(|s| s.last_error = Some(msg));
                Err(e)
            }
        }
    }

    /// Run the background poll loop until cancelled. Never returns `Err`:
    /// transient check/apply failures are logged and recorded in status, and
    /// the loop keeps polling. In `Auto` mode, App updates are applied
    /// automatically; OS/MCU updates are only *reported* (apply them explicitly,
    /// with dry-run gates honoured).
    pub async fn run(self) -> anyhow::Result<()> {
        if !self.enabled {
            log::info!("pod-updater disabled; not polling");
            std::future::pending::<()>().await;
            return Ok(());
        }
        if self.sources.is_empty() {
            log::warn!("pod-updater enabled but no sources configured; idling");
            std::future::pending::<()>().await;
            return Ok(());
        }
        log::info!(
            "pod-updater: channel={} mode={} every {:?} ({} source(s))",
            self.channel,
            self.mode.as_str(),
            self.poll_interval,
            self.sources.len(),
        );
        loop {
            match self.check().await {
                Ok(available) if available.is_empty() => {
                    log::debug!("pod-updater: up to date");
                }
                Ok(available) => {
                    log::info!(
                        "pod-updater: {} update(s) available: {}",
                        available.len(),
                        available
                            .iter()
                            .map(|c| format!("{:?}={}", c.kind, c.version))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    if self.mode == UpdateMode::Auto {
                        // Auto-apply the App tier only; OS/MCU stay manual.
                        if available.iter().any(|c| c.kind == ComponentKind::App) {
                            match self.apply(ComponentKind::App).await {
                                Ok(()) => log::info!("pod-updater: applied app update"),
                                Err(e) => log::error!("pod-updater: app apply failed: {e}"),
                            }
                        }
                    }
                }
                Err(e) => log::warn!("pod-updater: check failed: {e}"),
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    /// Build a fully-wired updater from [`UpdaterConfig`] using real transports
    /// (rustls HTTP / local dir), the system installer, and an HTTP canary.
    pub fn from_config(config: &UpdaterConfig) -> Result<Self> {
        let policy = config.trust.resolve()?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let sources: Vec<Box<dyn ReleaseSource>> = config
            .sources
            .iter()
            .map(|s| build_source(client.clone(), s.resolve(&config.manifest_name)))
            .collect();

        let layout = ReleaseLayout::new(config.paths.clone());
        let installer: Box<dyn ReleaseInstaller> = Box::new(SystemInstaller {
            service: "podd.service".into(),
        });
        let health: Box<dyn HealthCheck> = Box::new(HttpHealthCheck {
            client,
            url: config.health_url.clone(),
            timeout: config.health_timeout,
        });

        let status = UpdateStatus::new(
            config.enabled,
            config.channel.clone(),
            config.mode.as_str().into(),
        );
        let (status_tx, _) = watch::channel(status);

        Ok(Updater {
            channel: config.channel.clone(),
            mode: config.mode,
            policy,
            sources,
            staging_dir: config.paths.staging_dir.clone(),
            layout,
            installer,
            health,
            os_writer: Box::new(DryOsSlotWriter),
            mcu_flasher: Box::new(DryMcuFlasher),
            keep_releases: config.keep_releases,
            os_dry_run: config.os_dry_run,
            mcu_dry_run: config.mcu_dry_run,
            enabled: config.enabled,
            poll_interval: config.poll_interval,
            status_tx,
        })
    }
}

/// Build an updater from the environment and run its poll loop. Returns a future
/// suitable to hand to `tokio::try_join!` alongside the core + api futures: it
/// never resolves with `Err` for transient reasons, so it will not tear the
/// process down on a failed check.
pub async fn run_from_env() -> anyhow::Result<()> {
    let config = UpdaterConfig::from_env();
    if !config.enabled {
        log::info!("pod-updater disabled via config");
        std::future::pending::<()>().await;
        return Ok(());
    }
    match Updater::from_config(&config) {
        Ok(updater) => updater.run().await,
        Err(e) => {
            log::error!("pod-updater: failed to build ({e}); idling");
            std::future::pending::<()>().await;
            Ok(())
        }
    }
}

/// Helper for callers wanting a shared updater + its status receiver.
pub fn shared(config: &UpdaterConfig) -> Result<(Arc<Updater>, watch::Receiver<UpdateStatus>)> {
    let updater = Updater::from_config(config)?;
    let rx = updater.status_watch();
    Ok((Arc::new(updater), rx))
}
