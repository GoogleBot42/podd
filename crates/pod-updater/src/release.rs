//! Tier-2 atomic release swap: the on-disk state machine behind `apply` and
//! `rollback` for the app tier.
//!
//! Layout under [`UpdaterPaths::release_root`]:
//! ```text
//!   releases/
//!     <version-A>/app.squashfs   (+ rootfs/ once staged)
//!     <version-B>/app.squashfs
//!     versions.json              (installed version per tier, for status/check)
//!   current   -> releases/<version-B>   (atomic symlink; source of truth for app)
//!   previous  -> releases/<version-A>   (rollback target)
//! ```
//!
//! The swap is atomic because we only ever change what `current` points at, via
//! a create-temp-symlink-then-`rename` (atomic on a POSIX filesystem). A crash
//! at any point leaves either the old or the new `current` — never a
//! half-written tree.

use crate::config::UpdaterPaths;
use crate::error::{Error, Result};
use crate::install::{HealthCheck, ReleaseInstaller};
use pod_update::{Component, ComponentKind};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The `previous` symlink sits next to `current`.
fn previous_link(paths: &UpdaterPaths) -> PathBuf {
    paths.current_link.with_file_name(format!(
        "{}-previous",
        paths
            .current_link
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("current")
    ))
}

/// The bare filename of an app artifact within a release dir.
const APP_ARTIFACT: &str = "app.squashfs";
/// Installed-versions record (per tier) used for status + update detection.
const VERSIONS_FILE: &str = "versions.json";

/// Operates the release directory: install, activate, roll back, prune.
pub struct ReleaseLayout {
    pub paths: UpdaterPaths,
}

impl ReleaseLayout {
    pub fn new(paths: UpdaterPaths) -> Self {
        ReleaseLayout { paths }
    }

    fn previous(&self) -> PathBuf {
        previous_link(&self.paths)
    }

    fn versions_path(&self) -> PathBuf {
        self.paths.release_root.join(VERSIONS_FILE)
    }

    /// The version `current` points at (its directory basename), if any.
    pub fn current_app_version(&self) -> Option<String> {
        read_link_basename(&self.paths.current_link)
    }

    /// Installed version for a tier. App is read from the `current` symlink
    /// (the source of truth); other tiers from the versions record.
    pub fn installed_version(&self, kind: ComponentKind) -> Option<String> {
        if kind == ComponentKind::App {
            if let Some(v) = self.current_app_version() {
                return Some(v);
            }
        }
        self.read_versions().remove(&kind_key(kind))
    }

    fn read_versions(&self) -> BTreeMap<String, String> {
        match std::fs::read(self.versions_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => BTreeMap::new(),
        }
    }

    /// Record the installed version for a tier (best-effort, atomic write).
    pub fn record_version(&self, kind: ComponentKind, version: &str) -> Result<()> {
        let mut map = self.read_versions();
        map.insert(kind_key(kind), version.to_string());
        std::fs::create_dir_all(&self.paths.release_root)?;
        let json = serde_json::to_vec_pretty(&map)?;
        let tmp = self.versions_path().with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, self.versions_path())?;
        Ok(())
    }

    /// Install a verified app squashfs and activate it iff the canary passes.
    ///
    /// Flow: place the artifact under `releases/<version>/app.squashfs`, stage
    /// it (mount/extract), run the health check, and only then move `previous`
    /// to the outgoing release and flip `current` to the new one + restart. On
    /// a failed canary the new release directory is discarded and `current` is
    /// left exactly as it was.
    pub async fn install_app(
        &self,
        component: &Component,
        staged_squashfs: &Path,
        installer: &dyn ReleaseInstaller,
        health: &dyn HealthCheck,
        keep: usize,
    ) -> Result<()> {
        let version = &component.version;
        let release_dir = self.paths.release_root.join(version);
        std::fs::create_dir_all(&release_dir)?;
        let dest = release_dir.join(APP_ARTIFACT);
        move_file(staged_squashfs, &dest)?;

        // Prepare the release for execution, then exercise it.
        installer.stage(&release_dir, &dest)?;
        if !health.healthy().await {
            // Discard: the canary failed, keep the current release untouched.
            let _ = std::fs::remove_dir_all(&release_dir);
            return Err(Error::HealthCheckFailed {
                version: version.clone(),
            });
        }

        // Point `previous` at the outgoing release (for rollback), then flip
        // `current`. Both are atomic symlink swaps.
        if let Some(old) = read_link_target(&self.paths.current_link) {
            atomic_symlink(&old, &self.previous())?;
        }
        atomic_symlink(&release_dir, &self.paths.current_link)?;
        installer.restart()?;

        self.record_version(ComponentKind::App, version)?;
        self.prune(keep)?;
        Ok(())
    }

    /// Roll `current` back to the `previous` release and restart. Returns the
    /// version rolled back to.
    pub fn rollback(&self, installer: &dyn ReleaseInstaller) -> Result<String> {
        let prev = read_link_target(&self.previous()).ok_or(Error::NoPreviousRelease)?;
        if !prev.exists() {
            return Err(Error::ReleaseMissing(
                prev.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string(),
            ));
        }
        let outgoing = read_link_target(&self.paths.current_link);
        atomic_symlink(&prev, &self.paths.current_link)?;
        // Swap `previous` to the release we just left, so a second rollback
        // returns here (toggle between the two most recent).
        if let Some(outgoing) = outgoing {
            atomic_symlink(&outgoing, &self.previous())?;
        }
        installer.restart()?;
        let version = basename(&prev);
        self.record_version(ComponentKind::App, &version)?;
        Ok(version)
    }

    /// Keep the `keep` most-recent release dirs plus whatever `current` and
    /// `previous` point at; remove the rest.
    pub fn prune(&self, keep: usize) -> Result<()> {
        let root = &self.paths.release_root;
        let protected: Vec<String> = [
            read_link_basename(&self.paths.current_link),
            read_link_basename(&self.previous()),
        ]
        .into_iter()
        .flatten()
        .collect();

        // (path, mtime) for every release dir.
        let mut dirs: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            dirs.push((path, mtime));
        }
        // Newest first; retain the first `keep`.
        dirs.sort_by(|a, b| b.1.cmp(&a.1));
        let mut keep_names: std::collections::HashSet<String> =
            protected.into_iter().collect();
        for (path, _) in dirs.iter().take(keep) {
            keep_names.insert(basename(path));
        }
        for (path, _) in &dirs {
            if !keep_names.contains(&basename(path)) {
                let _ = std::fs::remove_dir_all(path);
            }
        }
        Ok(())
    }
}

/// Stable string key for the versions record.
fn kind_key(kind: ComponentKind) -> String {
    match kind {
        ComponentKind::App => "app",
        ComponentKind::Os => "os",
        ComponentKind::McuFrozen => "mcu_frozen",
        ComponentKind::McuSensor => "mcu_sensor",
        ComponentKind::Bootloader => "bootloader",
    }
    .to_string()
}

fn basename(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Read the target a symlink points at (absolute), or `None`.
fn read_link_target(link: &Path) -> Option<PathBuf> {
    std::fs::read_link(link).ok()
}

/// Read the basename of a symlink's target.
fn read_link_basename(link: &Path) -> Option<String> {
    read_link_target(link).map(|t| basename(&t))
}

/// Move a file, falling back to copy+remove across filesystems.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(from, to)?;
            let _ = std::fs::remove_file(from);
            Ok(())
        }
    }
}

/// Atomically point `link` at `target`: create a temp symlink then `rename`
/// over `link` (atomic replace on POSIX).
fn atomic_symlink(target: &Path, link: &Path) -> Result<()> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = link.with_file_name(format!(
        "{}.tmp",
        link.file_name().and_then(|s| s.to_str()).unwrap_or("link")
    ));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    std::fs::rename(&tmp, link)?;
    Ok(())
}
