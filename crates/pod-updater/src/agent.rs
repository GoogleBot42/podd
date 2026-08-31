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
//! 5. Dispatch by tier: App = atomic swap with a post-restart canary +
//!    automatic rollback (a two-phase trial — see [`crate::trial`]); OS/MCU =
//!    gated plumbing.

use crate::bootenv::{BootEnv, FwEnv};
use crate::config::{OsWriterKind, UpdateMode, UpdaterConfig};
use crate::error::{Error, Result};
use crate::install::{
    DryMcuFlasher, DryOsSlotWriter, HealthCheck, HttpHealthCheck, McuFlasher, OsSlotWriter,
    ReleaseInstaller, SystemInstaller,
};
use crate::os_slot::AbSlotWriter;
use crate::os_trial::{self, OsTrialOutcome};
use crate::release::ReleaseLayout;
use crate::source::{build_source, ReleaseSource};
use crate::status::{now_unix, AvailableUpdate, UpdateStatus, VersionEntry};
use crate::trial::{self, TrialOutcome};
use pod_update::{Component, ComponentKind, Manifest, SignedManifest, TrustPolicy};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

/// The systemd unit the app tier restarts after activation/rollback.
const PODD_SERVICE: &str = "podd.service";

/// The agent. Construct with [`Updater::from_config`] (real transports) or the
/// field-wise [`Updater::new`] + `with_*` setters (tests/custom wiring).
pub struct Updater {
    /// The release channel followed. Interior-mutable: the owner can switch
    /// channels at runtime ([`Updater::set_channel`]) without restarting podd,
    /// and the switch is persisted so it survives one.
    channel: std::sync::RwLock<String>,
    mode: UpdateMode,
    policy: TrustPolicy,
    sources: Vec<Box<dyn ReleaseSource>>,
    staging_dir: PathBuf,
    layout: ReleaseLayout,
    installer: Box<dyn ReleaseInstaller>,
    health: Box<dyn HealthCheck>,
    os_writer: Box<dyn OsSlotWriter>,
    boot_env: Arc<dyn BootEnv>,
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
        let mut status = UpdateStatus::new(true, channel.clone(), mode.as_str().into());
        status.current_versions = installed_versions(&layout);
        let (status_tx, _) = watch::channel(status);
        Updater {
            channel: std::sync::RwLock::new(channel),
            mode,
            policy,
            sources,
            staging_dir,
            layout,
            installer,
            health,
            os_writer: Box::new(DryOsSlotWriter),
            boot_env: Arc::new(FwEnv),
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
    pub fn with_boot_env(mut self, e: Arc<dyn BootEnv>) -> Self {
        self.boot_env = e;
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
    /// Switch the agent off (as `PODD_UPDATER_ENABLED=false` does): no polling,
    /// and [`Updater::apply`] refuses. Trial resolution still runs.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self.set_status(|s| s.enabled = enabled);
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

    /// Re-read the installed versions off disk into the published status,
    /// without contacting a release source. Lets the observability surface
    /// report what the device runs even when no source is configured or the
    /// last check failed.
    pub fn refresh_versions(&self) {
        let versions = installed_versions(&self.layout);
        self.set_status(|s| s.current_versions = versions);
    }

    fn set_status(&self, f: impl FnOnce(&mut UpdateStatus)) {
        self.status_tx.send_modify(f);
    }

    /// The release channel currently followed.
    pub fn channel(&self) -> String {
        self.channel.read().expect("channel lock").clone()
    }

    /// Switch the followed release channel at runtime and persist the choice
    /// (`<release_root>/channel.json`), so it survives a restart and outranks
    /// `PODD_UPDATER_CHANNEL` from then on. Returns the normalised name.
    ///
    /// Nothing is downloaded or applied here: the next check (poll tick or
    /// `POST /api/updates/check`) is what consults the new channel. Any
    /// previously-offered components are dropped from the published status —
    /// they describe the *old* channel and must not read as offers on the new
    /// one. Allowed while the agent is disabled: it only records a preference.
    pub fn set_channel(&self, channel: &str) -> Result<String> {
        let channel = crate::config::validate_channel(channel)?;
        // Persist first: a switch the owner is told succeeded must survive a
        // restart, and a read-only /opt must fail the request, not lie.
        crate::config::save_channel_override(&self.layout.paths, &channel)?;
        let changed = {
            let mut guard = self.channel.write().expect("channel lock");
            let changed = *guard != channel;
            guard.clone_from(&channel);
            changed
        };
        let published = channel.clone();
        self.set_status(move |s| {
            s.channel = published;
            if changed {
                // The old channel's offers and check verdict no longer apply.
                s.available.clear();
                s.last_check_unix = None;
                s.last_check_ok = false;
                s.last_error = None;
            }
        });
        if changed {
            log::info!("pod-updater: channel switched to {channel}");
        }
        Ok(channel)
    }

    /// Fetch and verify the manifest from the first source that yields a valid,
    /// channel-matching, trust-policy-satisfying manifest.
    async fn fetch_verified(&self) -> Result<Manifest> {
        let want_channel = self.channel();
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
                Ok(m) if m.channel == want_channel => return Ok(m),
                Ok(m) => {
                    last_err = format!(
                        "{}: channel mismatch (want {}, got {})",
                        src.label(),
                        want_channel,
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
                let versions = installed_versions(&self.layout);
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

    /// Apply the newest manifest's component of `kind`. App = atomic swap with
    /// a post-restart canary trial (see [`crate::trial`]); OS/MCU = verified +
    /// gated plumbing; Bootloader is refused.
    ///
    /// Refuses outright when the agent is disabled
    /// (`PODD_UPDATER_ENABLED=false`): an operator who switched the agent off
    /// must not get an update applied by pressing a button, and the refusal is
    /// explicit rather than a silent no-op. (The poll loop never reaches here
    /// when disabled — it stops before polling.)
    pub async fn apply(&self, kind: ComponentKind) -> Result<()> {
        if !self.enabled {
            return Err(Error::Disabled);
        }
        if kind == ComponentKind::Bootloader {
            return Err(Error::BootloaderRefused);
        }
        if kind == ComponentKind::App {
            // An explicit apply is consent to retry a previously rolled-back
            // release; only the auto-apply loop honours the failure marker.
            trial::clear_failure(&self.layout.paths);
        }
        let manifest = self.fetch_verified().await?;
        let component = manifest
            .component(kind)
            .ok_or(Error::ComponentMissing(kind))?
            .clone();
        // Enforce the manifest's min_app dependency before any download or
        // dispatch — an OS/MCU component built against a newer app must not
        // land under an older one (#40). Fails closed on unknown versions.
        let installed_app = self.layout.installed_version(ComponentKind::App);
        if !component.min_app_satisfied(installed_app.as_deref()) {
            return Err(Error::MinAppNotMet {
                name: component.name.clone(),
                min_app: component.min_app.clone().unwrap_or_default(),
                installed: installed_app,
            });
        }
        let staged = self.download_verified(&manifest, &component).await?;

        let outcome = match kind {
            ComponentKind::App => {
                // On-device the restart inside install_app kills this process;
                // the new process resolves the trial (commit or rollback).
                self.layout
                    .install_app(&component, &staged, &*self.installer)
                    .await
                    .map(|_| format!("app -> {} (trial; canary after restart)", component.version))
            }
            ComponentKind::Os => self
                .os_writer
                .write_inactive_slot(&component, &staged, self.os_dry_run)
                .await
                .map(|_| {
                    // Never record the version here: a live write only staged
                    // + armed the slot — the version is recorded by the OS
                    // trial resolution after the first healthy boot of the
                    // new slot (and a dry-run applied nothing at all, #39).
                    // Until then check()/status keep reporting it pending.
                    format!(
                        "os -> {} ({})",
                        component.version,
                        if self.os_dry_run {
                            "dry-run"
                        } else {
                            "written to inactive slot + armed; reboot to activate"
                        }
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

    /// Resolve a pending app trial (see [`crate::trial`]): canary this
    /// process's own API, then commit or roll back + restart. Returns `None`
    /// when no trial is pending. Called at the start of [`Updater::run`]; also
    /// callable directly from tests/custom wiring.
    pub async fn resolve_pending_trial(&self) -> Option<TrialOutcome> {
        let outcome =
            trial::resolve_trial(&self.layout, &*self.installer, &*self.health, self.keep_releases)
                .await?;
        match &outcome {
            TrialOutcome::Committed { version } => self.set_status(|s| {
                s.last_applied = Some(format!("app -> {version} (committed)"));
                s.last_error = None;
            }),
            TrialOutcome::RolledBack { version, restored } => self.set_status(|s| {
                s.last_error = Some(format!(
                    "app {version} rolled back after failed canary (restored {})",
                    restored.as_deref().unwrap_or("?")
                ));
            }),
            TrialOutcome::Abandoned { version } => self.set_status(|s| {
                s.last_error = Some(format!(
                    "app {version} failed its canary; no previous release to restore"
                ));
            }),
        }
        Some(outcome)
    }

    /// Resolve the OS-tier trial (see [`crate::os_trial`]): after a healthy
    /// boot, disarm the U-Boot env (mark-good) and record the committed OS
    /// version; detect a U-Boot auto-revert. `None` when there is nothing to
    /// do (env unreadable / not an A/B system / nothing pending).
    pub async fn resolve_os_trial(&self) -> Option<OsTrialOutcome> {
        let outcome =
            os_trial::resolve_os_trial(&self.layout, &*self.boot_env, &*self.health).await?;
        match &outcome {
            OsTrialOutcome::Committed { version } => self.set_status(|s| {
                s.last_applied = Some(format!("os -> {version} (committed after healthy boot)"));
                s.last_error = None;
            }),
            OsTrialOutcome::RolledBack { version } => self.set_status(|s| {
                s.last_error = Some(format!(
                    "os {version} did not activate (U-Boot rolled back / trial cancelled)"
                ));
            }),
            OsTrialOutcome::Disarmed | OsTrialOutcome::StillArmed
            | OsTrialOutcome::AwaitingReboot => {}
        }
        Some(outcome)
    }

    /// Like [`Self::resolve_os_trial`], but keeps retrying while the trial is
    /// armed-and-unhealthy (the disarm must eventually happen or U-Boot will
    /// revert an actually-good slot after 3 more reboots). Returns once the
    /// trial settles or turns out to be idle.
    async fn resolve_os_trial_until_settled(&self) {
        loop {
            match self.resolve_os_trial().await {
                Some(OsTrialOutcome::StillArmed) => {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
                _ => return,
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
    pub async fn run(&self) -> anyhow::Result<()> {
        // A half-finished activation must be resolved before anything else —
        // this process may itself be the new release on trial. Same for the
        // OS tier: after the activation reboot, this boot must mark-good (or
        // U-Boot will revert a perfectly good slot after 3 more reboots).
        self.resolve_pending_trial().await;
        self.resolve_os_trial_until_settled().await;
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
            self.channel(),
            self.mode.as_str(),
            self.poll_interval,
            self.sources.len(),
        );
        loop {
            // Cheap when idle (one env read / marker stat); keeps mark-good
            // and revert detection working across poll ticks (e.g. an arm
            // done by a manual apply between ticks, resolved after the
            // owner's reboot without a podd restart in between).
            self.resolve_os_trial().await;
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
                        if let Some(app) =
                            available.iter().find(|c| c.kind == ComponentKind::App)
                        {
                            // Never auto-retry a release that was rolled back:
                            // each retry restarts podd twice, and every restart
                            // opens the sensor MCU's ~60 s ignore-writes window.
                            let failed = trial::last_failure(&self.layout.paths)
                                .is_some_and(|f| f.version == app.version);
                            if failed {
                                log::warn!(
                                    "pod-updater: skipping app {} — its last activation was \
                                     rolled back; apply manually to retry",
                                    app.version
                                );
                            } else {
                                match self.apply(ComponentKind::App).await {
                                    Ok(()) => log::info!("pod-updater: applied app update"),
                                    Err(e) => log::error!("pod-updater: app apply failed: {e}"),
                                }
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
            service: PODD_SERVICE.into(),
        });
        let health: Box<dyn HealthCheck> = Box::new(HttpHealthCheck {
            client,
            url: config.health_url.clone(),
            timeout: config.health_timeout,
        });

        let mut status = UpdateStatus::new(
            config.enabled,
            config.channel.clone(),
            config.mode.as_str().into(),
        );
        status.current_versions = installed_versions(&layout);
        let (status_tx, _) = watch::channel(status);

        let boot_env: Arc<dyn BootEnv> = Arc::new(FwEnv);
        let os_writer = select_os_writer(config, boot_env.clone());

        Ok(Updater {
            channel: std::sync::RwLock::new(config.channel.clone()),
            mode: config.mode,
            policy,
            sources,
            staging_dir: config.paths.staging_dir.clone(),
            layout,
            installer,
            health,
            os_writer,
            boot_env,
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

/// Installed versions per tier, read off disk (App from the `current` symlink;
/// the other tiers from the recorded `versions.json`). Tiers that were never
/// installed through the updater are simply absent.
fn installed_versions(layout: &ReleaseLayout) -> Vec<VersionEntry> {
    [
        ComponentKind::App,
        ComponentKind::Os,
        ComponentKind::McuFrozen,
        ComponentKind::McuSensor,
        ComponentKind::Bootloader,
    ]
    .into_iter()
    .filter_map(|kind| {
        layout
            .installed_version(kind)
            .map(|version| VersionEntry { kind, version })
    })
    .collect()
}

/// Pick the Tier-1 OS writer per config. `Auto` requires the on-device A/B
/// contract to be visibly present — the fw_env.config the env tools need AND
/// the slot-2 block device — and falls back to the plan-only dry writer
/// otherwise (dev boxes, non-A/B installs).
fn select_os_writer(config: &UpdaterConfig, boot_env: Arc<dyn BootEnv>) -> Box<dyn OsSlotWriter> {
    let live = || -> Box<dyn OsSlotWriter> {
        Box::new(AbSlotWriter::mmc(
            boot_env.clone(),
            config.paths.release_root.clone(),
        ))
    };
    match config.os_writer {
        OsWriterKind::Dry => Box::new(DryOsSlotWriter),
        OsWriterKind::Mmc => live(),
        OsWriterKind::Auto => {
            let has_hw = std::path::Path::new("/etc/fw_env.config").exists()
                && std::path::Path::new(crate::os_slot::MMC_SLOT_DEVICES[1]).exists();
            if has_hw {
                live()
            } else {
                Box::new(DryOsSlotWriter)
            }
        }
    }
}

/// Build an updater from the environment and run its poll loop. Returns a future
/// suitable to hand to `tokio::try_join!` alongside the core + api futures: it
/// never resolves with `Err` for transient reasons, so it will not tear the
/// process down on a failed check.
pub async fn run_from_env() -> anyhow::Result<()> {
    let (_updater, run) = from_env_shared();
    run.await
}

/// [`run_from_env`] split in two, for callers that also want to *observe* and
/// drive the agent (the `api` crate's update panel): the shared handle plus the
/// future running its poll loop. The handle is `None` only when the agent could
/// not be built at all (bad trust config) — the future still resolves any
/// pending trial and then idles, exactly as [`run_from_env`] does.
///
/// A disabled agent (`PODD_UPDATER_ENABLED=false`) still yields a handle: its
/// status is worth showing, and [`Updater::run`] handles the not-polling case.
#[allow(clippy::type_complexity)]
pub fn from_env_shared() -> (
    Option<Arc<Updater>>,
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>,
) {
    let config = UpdaterConfig::from_env();
    match Updater::from_config(&config) {
        Ok(updater) => {
            let updater = Arc::new(updater);
            let run = updater.clone();
            (Some(updater), Box::pin(async move { run.run().await }))
        }
        Err(e) => (
            None,
            Box::pin(async move {
                // Even with no usable config, a half-finished activation (this
                // process may be a new release on trial) must be committed or
                // rolled back.
                resolve_trial_standalone(&config).await;
                log::error!("pod-updater: failed to build ({e}); idling");
                std::future::pending::<()>().await;
                Ok(())
            }),
        ),
    }
}

/// Resolve pending app AND OS trials without a full [`Updater`] (used when
/// the updater is disabled or its config is broken — trial resolution must
/// not depend on sources/trust being configured).
async fn resolve_trial_standalone(config: &UpdaterConfig) {
    let has_app_trial = trial::load(&config.paths).is_some();
    let layout = ReleaseLayout::new(config.paths.clone());
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("pod-updater: cannot build health-check client ({e}); trial unresolved");
            return;
        }
    };
    let health = HttpHealthCheck {
        client,
        url: config.health_url.clone(),
        timeout: config.health_timeout,
    };
    if has_app_trial {
        let installer = SystemInstaller {
            service: PODD_SERVICE.into(),
        };
        trial::resolve_trial(&layout, &installer, &health, config.keep_releases).await;
    }
    // OS mark-good must run even with the updater disabled — otherwise a
    // healthy new slot would be reverted by U-Boot after 3 more reboots.
    // Retry while armed-and-unhealthy, same as Updater::run.
    let env = FwEnv;
    loop {
        match os_trial::resolve_os_trial(&layout, &env, &health).await {
            Some(OsTrialOutcome::StillArmed) => {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
            _ => return,
        }
    }
}

/// Helper for callers wanting a shared updater + its status receiver.
pub fn shared(config: &UpdaterConfig) -> Result<(Arc<Updater>, watch::Receiver<UpdateStatus>)> {
    let updater = Updater::from_config(config)?;
    let rx = updater.status_watch();
    Ok((Arc::new(updater), rx))
}
