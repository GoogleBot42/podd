//! Release transport: fetching the signed manifest and artifacts.
//!
//! All fetching goes through the [`ReleaseSource`] trait so the agent's
//! verify/apply logic is testable against an in-memory source with no network.
//! Two real implementations ship: [`HttpSource`] (GitHub/Gitea/self-hosted over
//! rustls) and [`LocalDirSource`] (LAN mount / USB — the offline path).
//!
//! A source only *transports* bytes; it makes **no** trust decision. The agent
//! verifies the manifest signature and every artifact's size+digest after
//! fetching (see [`crate::Updater`]). `fetch_artifact` writes to a `.part` file
//! and renames on completion, so a caller never sees a truncated file.

use crate::config::ResolvedSource;
use crate::error::{Error, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Transport for a signed manifest and its artifacts. Integrity/authenticity
/// are enforced by the caller, never here.
#[async_trait]
pub trait ReleaseSource: Send + Sync {
    /// A short label for logs/errors (e.g. the manifest URL or a dir path).
    fn label(&self) -> String;

    /// Fetch the raw signed-manifest JSON.
    async fn fetch_manifest(&self) -> Result<String>;

    /// Download `filename` to `dest`, writing the complete file or erroring.
    async fn fetch_artifact(&self, filename: &str, dest: &Path) -> Result<()>;
}

/// Build the appropriate concrete source for a [`ResolvedSource`].
pub fn build_source(client: reqwest::Client, resolved: ResolvedSource) -> Box<dyn ReleaseSource> {
    match resolved {
        ResolvedSource::Http {
            manifest_url,
            artifact_base_url,
        } => Box::new(HttpSource {
            client,
            manifest_url,
            artifact_base_url,
        }),
        ResolvedSource::Local { dir } => Box::new(LocalDirSource { dir }),
    }
}

/// Fetch over HTTP(S) using a shared rustls-backed reqwest client.
pub struct HttpSource {
    pub client: reqwest::Client,
    pub manifest_url: String,
    pub artifact_base_url: String,
}

#[async_trait]
impl ReleaseSource for HttpSource {
    fn label(&self) -> String {
        self.manifest_url.clone()
    }

    async fn fetch_manifest(&self) -> Result<String> {
        let resp = self.client.get(&self.manifest_url).send().await?;
        let resp = resp.error_for_status()?;
        Ok(resp.text().await?)
    }

    async fn fetch_artifact(&self, filename: &str, dest: &Path) -> Result<()> {
        let url = format!("{}/{}", self.artifact_base_url, filename);
        let resp = self.client.get(&url).send().await?;
        let bytes = resp.error_for_status()?.bytes().await?;
        write_atomic(dest, &bytes).await
    }
}

/// Read a release from a local directory (LAN mount / USB — offline install).
pub struct LocalDirSource {
    pub dir: PathBuf,
}

#[async_trait]
impl ReleaseSource for LocalDirSource {
    fn label(&self) -> String {
        self.dir.display().to_string()
    }

    async fn fetch_manifest(&self) -> Result<String> {
        // The manifest name is fixed at the config level; local sources always
        // look for `manifest.json` in the directory root.
        let path = self.dir.join(crate::config::DEFAULT_MANIFEST_NAME);
        Ok(tokio::fs::read_to_string(path).await?)
    }

    async fn fetch_artifact(&self, filename: &str, dest: &Path) -> Result<()> {
        let src = self.dir.join(filename);
        let bytes = tokio::fs::read(&src).await?;
        write_atomic(dest, &bytes).await
    }
}

/// An in-memory source for tests: a fixed manifest string plus a filename→bytes
/// map. Also records which artifacts were requested.
pub struct MemorySource {
    pub manifest: String,
    pub artifacts: HashMap<String, Vec<u8>>,
    pub label: String,
    pub requested: Mutex<Vec<String>>,
}

impl MemorySource {
    pub fn new(manifest: impl Into<String>) -> Self {
        MemorySource {
            manifest: manifest.into(),
            artifacts: HashMap::new(),
            label: "memory".into(),
            requested: Mutex::new(Vec::new()),
        }
    }

    pub fn with_artifact(mut self, filename: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        self.artifacts.insert(filename.into(), bytes.into());
        self
    }
}

#[async_trait]
impl ReleaseSource for MemorySource {
    fn label(&self) -> String {
        self.label.clone()
    }

    async fn fetch_manifest(&self) -> Result<String> {
        Ok(self.manifest.clone())
    }

    async fn fetch_artifact(&self, filename: &str, dest: &Path) -> Result<()> {
        self.requested.lock().unwrap().push(filename.to_string());
        let bytes = self
            .artifacts
            .get(filename)
            .ok_or_else(|| Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no such artifact in memory source: {filename}"),
            )))?;
        write_atomic(dest, bytes).await
    }
}

/// Write `bytes` to `dest` via a sibling `.part` file, renamed on completion so
/// a partially-written file is never exposed at `dest`.
async fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let part = dest.with_extension("part");
    tokio::fs::write(&part, bytes).await?;
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}
