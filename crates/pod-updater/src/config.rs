//! Owner-facing configuration for the update agent.
//!
//! The design honours `pod-update`'s trust model: **integrity (SHA-256) is
//! always enforced**, while **authenticity (a signature) is owner-controlled**
//! via [`TrustConfig`]. Everything here is plain data + a couple of `resolve`
//! helpers that turn config into runtime objects (a [`TrustPolicy`], a list of
//! resolved source endpoints).

use crate::error::{Error, Result};
use pod_update::sign::{decode_verifying_key, VerifyingKey};
use pod_update::TrustPolicy;
use std::path::PathBuf;
use std::time::Duration;

/// Default filename of the signed manifest within a release.
pub const DEFAULT_MANIFEST_NAME: &str = "manifest.json";

/// Whether the loop applies updates on its own or only reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateMode {
    /// Poll and (for auto-appliable tiers) apply without operator action.
    Auto,
    /// Poll and report only; `apply()` must be triggered explicitly.
    Manual,
}

impl UpdateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateMode::Auto => "auto",
            UpdateMode::Manual => "manual",
        }
    }
}

/// Where to find a pinned public key the owner trusts.
#[derive(Clone, Debug)]
pub enum PubKeySource {
    /// A base64-encoded verifying key, inline in config.
    Inline(String),
    /// A path to a file containing the base64 verifying key (e.g. `signing.pub`).
    File(PathBuf),
}

impl PubKeySource {
    fn load(&self) -> Result<VerifyingKey> {
        let b64 = match self {
            PubKeySource::Inline(s) => s.clone(),
            PubKeySource::File(p) => std::fs::read_to_string(p)?,
        };
        decode_verifying_key(b64.trim()).map_err(Error::Core)
    }
}

/// The owner's trust decision. Mirrors [`TrustPolicy`] but carries *unresolved*
/// key sources (files/inline) so config can name keys by path.
#[derive(Clone, Debug)]
pub enum TrustConfig {
    /// Accept any manifest; artifact digests are still enforced.
    Unsigned,
    /// Require a valid signature from one of these owner-chosen keys.
    Signed(Vec<PubKeySource>),
}

impl TrustConfig {
    /// Load any key files and produce the runtime [`TrustPolicy`].
    pub fn resolve(&self) -> Result<TrustPolicy> {
        match self {
            TrustConfig::Unsigned => Ok(TrustPolicy::AllowUnsigned),
            TrustConfig::Signed(sources) => {
                if sources.is_empty() {
                    return Err(Error::Config(
                        "trust=signed but no trusted keys were configured".into(),
                    ));
                }
                let keys = sources
                    .iter()
                    .map(PubKeySource::load)
                    .collect::<Result<Vec<_>>>()?;
                Ok(TrustPolicy::RequireSigned(keys))
            }
        }
    }
}

/// A place the device fetches releases from. Supports explicit URLs plus the
/// GitHub and Gitea/Forgejo "Releases assets" URL shapes, and a local/USB dir
/// for offline installs.
#[derive(Clone, Debug)]
pub enum ReleaseSourceUrl {
    /// Fully explicit: a manifest URL and the base URL artifacts hang off.
    Explicit {
        manifest_url: String,
        artifact_base_url: String,
    },
    /// GitHub Releases: `https://github.com/{owner}/{repo}/releases/...`.
    /// `tag = None` uses the `.../releases/latest/download/` convenience path.
    GitHub {
        owner: String,
        repo: String,
        tag: Option<String>,
    },
    /// Gitea/Forgejo Releases on a self-hosted host, e.g.
    /// `https://git.example.org/{owner}/{repo}/releases/download/{tag}/...`.
    Gitea {
        /// Scheme + host, no trailing slash (e.g. `https://git.example.org`).
        base_url: String,
        owner: String,
        repo: String,
        tag: String,
    },
    /// A local directory (LAN mount / USB stick) holding the manifest + artifacts.
    LocalDir { dir: PathBuf },
}

/// A source resolved to concrete endpoints the fetch layer can use.
#[derive(Clone, Debug)]
pub enum ResolvedSource {
    Http {
        manifest_url: String,
        artifact_base_url: String,
    },
    Local {
        dir: PathBuf,
    },
}

impl ReleaseSourceUrl {
    /// Resolve to concrete endpoints. `manifest_name` is the manifest filename
    /// (e.g. `manifest.json`) used to build the manifest URL for hosted shapes.
    pub fn resolve(&self, manifest_name: &str) -> ResolvedSource {
        let trim = |s: &str| s.trim_end_matches('/').to_string();
        match self {
            ReleaseSourceUrl::Explicit {
                manifest_url,
                artifact_base_url,
            } => ResolvedSource::Http {
                manifest_url: manifest_url.clone(),
                artifact_base_url: trim(artifact_base_url),
            },
            ReleaseSourceUrl::GitHub { owner, repo, tag } => {
                let base = match tag {
                    Some(t) => format!(
                        "https://github.com/{owner}/{repo}/releases/download/{t}"
                    ),
                    None => format!(
                        "https://github.com/{owner}/{repo}/releases/latest/download"
                    ),
                };
                ResolvedSource::Http {
                    manifest_url: format!("{base}/{manifest_name}"),
                    artifact_base_url: base,
                }
            }
            ReleaseSourceUrl::Gitea {
                base_url,
                owner,
                repo,
                tag,
            } => {
                let base = format!(
                    "{}/{owner}/{repo}/releases/download/{tag}",
                    trim(base_url)
                );
                ResolvedSource::Http {
                    manifest_url: format!("{base}/{manifest_name}"),
                    artifact_base_url: base,
                }
            }
            ReleaseSourceUrl::LocalDir { dir } => ResolvedSource::Local { dir: dir.clone() },
        }
    }
}

/// Filesystem layout for the atomic release-swap machinery (Tier 2).
#[derive(Clone, Debug)]
pub struct UpdaterPaths {
    /// Root holding one directory per installed release (`<root>/<version>`).
    pub release_root: PathBuf,
    /// The `current` symlink the running system boots off.
    pub current_link: PathBuf,
    /// Where partially-downloaded artifacts are staged and verified.
    pub staging_dir: PathBuf,
}

impl UpdaterPaths {
    /// Conventional layout rooted at `/opt/podd` (see REPLACEMENT_PLAN §9).
    pub fn opt_podd() -> Self {
        UpdaterPaths {
            release_root: PathBuf::from("/opt/podd/releases"),
            current_link: PathBuf::from("/opt/podd/current"),
            staging_dir: PathBuf::from("/opt/podd/staging"),
        }
    }
}

/// Which Tier-1 (OS) writer to wire up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsWriterKind {
    /// Pick [`crate::os_slot::AbSlotWriter`] iff the A/B hardware contract is
    /// present (`/etc/fw_env.config` + the slot-2 block device), else the dry
    /// writer. The right default everywhere.
    Auto,
    /// Always the plan-only [`crate::install::DryOsSlotWriter`].
    Dry,
    /// Always the live SD writer (still gated by `os_dry_run`).
    Mmc,
}

/// The full updater configuration.
#[derive(Clone, Debug)]
pub struct UpdaterConfig {
    pub enabled: bool,
    pub channel: String,
    pub mode: UpdateMode,
    /// One or more sources, tried in order until one yields a verified manifest.
    pub sources: Vec<ReleaseSourceUrl>,
    /// Manifest filename within a release (default `manifest.json`).
    pub manifest_name: String,
    pub poll_interval: Duration,
    /// Owner trust decision (unsigned-ok or a set of trusted keys).
    pub trust: TrustConfig,
    pub paths: UpdaterPaths,
    /// How many recent releases to retain for instant rollback.
    pub keep_releases: usize,
    /// Canary health-check budget after activating a new app release.
    pub health_timeout: Duration,
    /// Gate destructive OS (Tier 1) writes. Default true.
    pub os_dry_run: bool,
    /// Which OS slot writer to use (default [`OsWriterKind::Auto`]).
    pub os_writer: OsWriterKind,
    /// Gate destructive MCU (Tier 3) flashes. Default true.
    pub mcu_dry_run: bool,
    /// Local API base used by the default HTTP health check (app canary).
    pub health_url: String,
}

impl Default for UpdaterConfig {
    fn default() -> Self {
        UpdaterConfig {
            enabled: true,
            channel: "stable".into(),
            mode: UpdateMode::Manual,
            sources: Vec::new(),
            manifest_name: DEFAULT_MANIFEST_NAME.into(),
            poll_interval: Duration::from_secs(3600),
            trust: TrustConfig::Unsigned,
            paths: UpdaterPaths::opt_podd(),
            keep_releases: 3,
            health_timeout: Duration::from_secs(20),
            os_dry_run: true,
            os_writer: OsWriterKind::Auto,
            mcu_dry_run: true,
            health_url: "http://127.0.0.1:3000/api/serverStatus".into(),
        }
    }
}

impl UpdaterConfig {
    /// Build a config from environment variables. Returns [`UpdaterConfig`] with
    /// `enabled=false` when the operator opts out (`PODD_UPDATER_ENABLED=false`).
    ///
    /// Recognised vars (all optional):
    /// - `PODD_UPDATER_ENABLED` (`false`/`0` disables; default enabled)
    /// - `PODD_UPDATER_CHANNEL` (default `stable`)
    /// - `PODD_UPDATER_MODE` (`auto`/`manual`; default `manual`)
    /// - `PODD_UPDATER_POLL_SECS` (default 3600)
    /// - `PODD_UPDATER_MANIFEST_URL` + `PODD_UPDATER_ARTIFACT_BASE` (explicit)
    /// - `PODD_UPDATER_GITHUB` = `owner/repo[@tag]`
    /// - `PODD_UPDATER_GITEA` = `https://host/owner/repo[@tag]`
    /// - `PODD_UPDATER_LOCAL_DIR` = a directory path
    /// - `PODD_UPDATER_TRUST` = `unsigned` | comma-separated pubkey file paths
    /// - `PODD_UPDATER_RELEASE_ROOT` / `_CURRENT` / `_STAGING`
    /// - `PODD_UPDATER_KEEP` (default 3)
    /// - `PODD_UPDATER_OS_DRY_RUN` / `_MCU_DRY_RUN` (`false`/`0` arms live apply)
    /// - `PODD_UPDATER_OS_WRITER` (`auto`/`dry`/`mmc`; default `auto`)
    /// - `PODD_UPDATER_HEALTH_URL`
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok();
        let mut cfg = UpdaterConfig {
            enabled: !matches!(env("PODD_UPDATER_ENABLED").as_deref(), Some("false") | Some("0")),
            ..UpdaterConfig::default()
        };

        if let Some(c) = env("PODD_UPDATER_CHANNEL") {
            cfg.channel = c;
        }
        if let Some(m) = env("PODD_UPDATER_MODE") {
            cfg.mode = if m.eq_ignore_ascii_case("auto") {
                UpdateMode::Auto
            } else {
                UpdateMode::Manual
            };
        }
        if let Some(s) = env("PODD_UPDATER_POLL_SECS").and_then(|v| v.parse::<u64>().ok()) {
            cfg.poll_interval = Duration::from_secs(s);
        }
        if let Some(k) = env("PODD_UPDATER_KEEP").and_then(|v| v.parse::<usize>().ok()) {
            cfg.keep_releases = k.max(1);
        }

        // Sources (accumulate any that are configured; order = priority).
        let mut sources = Vec::new();
        if let (Some(m), Some(a)) = (
            env("PODD_UPDATER_MANIFEST_URL"),
            env("PODD_UPDATER_ARTIFACT_BASE"),
        ) {
            sources.push(ReleaseSourceUrl::Explicit {
                manifest_url: m,
                artifact_base_url: a,
            });
        }
        if let Some(gh) = env("PODD_UPDATER_GITHUB") {
            if let Some(src) = parse_owner_repo_tag(&gh, None) {
                sources.push(src);
            }
        }
        if let Some(gt) = env("PODD_UPDATER_GITEA") {
            if let Some(src) = parse_gitea(&gt) {
                sources.push(src);
            }
        }
        if let Some(d) = env("PODD_UPDATER_LOCAL_DIR") {
            sources.push(ReleaseSourceUrl::LocalDir { dir: PathBuf::from(d) });
        }
        cfg.sources = sources;

        // Trust.
        if let Some(t) = env("PODD_UPDATER_TRUST") {
            let t = t.trim();
            if t.is_empty() || t.eq_ignore_ascii_case("unsigned") {
                cfg.trust = TrustConfig::Unsigned;
            } else {
                let keys = t
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|p| PubKeySource::File(PathBuf::from(p)))
                    .collect::<Vec<_>>();
                cfg.trust = TrustConfig::Signed(keys);
            }
        }

        // Paths.
        if let Some(p) = env("PODD_UPDATER_RELEASE_ROOT") {
            cfg.paths.release_root = PathBuf::from(p);
        }
        if let Some(p) = env("PODD_UPDATER_CURRENT") {
            cfg.paths.current_link = PathBuf::from(p);
        }
        if let Some(p) = env("PODD_UPDATER_STAGING") {
            cfg.paths.staging_dir = PathBuf::from(p);
        }

        // Destructive-write gates (default true = safe).
        cfg.os_dry_run = !matches!(
            env("PODD_UPDATER_OS_DRY_RUN").as_deref(),
            Some("false") | Some("0")
        );
        cfg.mcu_dry_run = !matches!(
            env("PODD_UPDATER_MCU_DRY_RUN").as_deref(),
            Some("false") | Some("0")
        );
        if let Some(w) = env("PODD_UPDATER_OS_WRITER") {
            cfg.os_writer = match w.to_ascii_lowercase().as_str() {
                "dry" => OsWriterKind::Dry,
                "mmc" => OsWriterKind::Mmc,
                _ => OsWriterKind::Auto,
            };
        }
        if let Some(u) = env("PODD_UPDATER_HEALTH_URL") {
            cfg.health_url = u;
        }
        cfg
    }
}

/// Parse `owner/repo[@tag]` into a [`ReleaseSourceUrl::GitHub`].
fn parse_owner_repo_tag(s: &str, _host: Option<&str>) -> Option<ReleaseSourceUrl> {
    let (path, tag) = match s.split_once('@') {
        Some((p, t)) => (p, Some(t.to_string())),
        None => (s, None),
    };
    let (owner, repo) = path.split_once('/')?;
    Some(ReleaseSourceUrl::GitHub {
        owner: owner.to_string(),
        repo: repo.to_string(),
        tag,
    })
}

/// Parse `https://host/owner/repo[@tag]` into a [`ReleaseSourceUrl::Gitea`].
fn parse_gitea(s: &str) -> Option<ReleaseSourceUrl> {
    let (rest, tag) = match s.rsplit_once('@') {
        // Only treat the trailing `@x` as a tag if it isn't part of the scheme.
        Some((p, t)) if !t.contains('/') => (p, t.to_string()),
        _ => (s, "latest".to_string()),
    };
    // Split scheme off, then take host / owner / repo.
    let (scheme, hostpath) = match rest.split_once("://") {
        Some((sch, hp)) => (sch, hp),
        None => ("https", rest),
    };
    let mut parts = hostpath.splitn(3, '/');
    let host = parts.next()?;
    let owner = parts.next()?;
    let repo = parts.next()?;
    Some(ReleaseSourceUrl::Gitea {
        base_url: format!("{scheme}://{host}"),
        owner: owner.to_string(),
        repo: repo.to_string(),
        tag,
    })
}
