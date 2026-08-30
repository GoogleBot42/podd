//! The update manifest: the signed, versioned description of what a release
//! contains. A manifest lists one [`Component`] per updatable part of the
//! system (see [`ComponentKind`]); the device fetches the manifest, verifies
//! its signature (see [`crate::sign`]), then verifies each artifact's digest
//! before applying it.
//!
//! The manifest is built only from structs and `Vec`s (no maps), so
//! `serde_json` serialization is deterministic — that is what makes
//! [`Manifest::canonical_bytes`] a stable input to sign and verify.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// The updatable parts of the system, each with its own cadence and mechanism.
/// See `REPLACEMENT_PLAN.md` §9 for the tiering rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// Tier 2: podd + web UI + config schema. Frequent, atomic, no reboot.
    App,
    /// Tier 1: full OS image (kernel+dtb+rootfs). A/B slots + bootloader failover.
    Os,
    /// Tier 3: STM32 "Frozen" MCU firmware (.bbin).
    McuFrozen,
    /// Tier 3: STM32 "Sensor" MCU firmware (.bbin).
    McuSensor,
    /// Tier 0: bootloader. Never auto-updated; present only for record-keeping.
    Bootloader,
}

/// A content-addressed artifact: the file the device must fetch and verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// Bare filename, resolved relative to the manifest's location or a base URL.
    pub filename: String,
    /// Lowercase hex SHA-256 of the artifact bytes.
    pub sha256: String,
    /// Size in bytes (checked before hashing to fail fast on truncation).
    pub size: u64,
}

/// One updatable component within a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    /// Human-readable name, e.g. "podd" or "os-imx8mm".
    pub name: String,
    pub kind: ComponentKind,
    /// Opaque version string (recommend: git tag + short closure hash).
    pub version: String,
    pub artifact: Artifact,
    /// Optional minimum app version required to apply this component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_app: Option<String>,
}

impl Component {
    /// Whether the installed app version satisfies this component's `min_app`
    /// requirement. No `min_app` always passes. When one is set, comparison
    /// uses the leading dotted-numeric part of both versions and **fails
    /// closed**: an unknown installed version or a non-numeric version on
    /// either side refuses the apply (this gate exists to stop
    /// incompatibility, so uncertainty must not pass).
    pub fn min_app_satisfied(&self, installed_app: Option<&str>) -> bool {
        let Some(min) = &self.min_app else {
            return true;
        };
        match (
            numeric_version_key(min),
            installed_app.and_then(numeric_version_key),
        ) {
            (Some(min_key), Some(installed_key)) => installed_key >= min_key,
            _ => false,
        }
    }
}

/// Leading dotted-numeric key of a version string, for ordering: an optional
/// `v` prefix is stripped and parsing stops at the first character that is
/// neither a digit nor a dot (`"v0.0.1-gfd93925"` → `[0, 0, 1]`). Returns
/// `None` when there is no clean leading number.
fn numeric_version_key(version: &str) -> Option<Vec<u64>> {
    let v = version.trim();
    let v = v.strip_prefix('v').unwrap_or(v);
    let end = v
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(v.len());
    let lead = &v[..end];
    if lead.is_empty() {
        return None;
    }
    lead.split('.').map(|part| part.parse().ok()).collect()
}

/// A release: one entry per component the channel currently offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    /// Release channel, e.g. "stable" or "beta".
    pub channel: String,
    /// Unix seconds the manifest was generated (metadata; covered by signature).
    pub generated_unix: i64,
    pub components: Vec<Component>,
}

impl Manifest {
    pub fn new(channel: impl Into<String>, generated_unix: i64) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            channel: channel.into(),
            generated_unix,
            components: Vec::new(),
        }
    }

    /// Deterministic byte representation used as the signing/verifying input.
    /// Stable because the manifest contains no maps (field + vec order only).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// The (first) component of a given kind, if present.
    pub fn component(&self, kind: ComponentKind) -> Option<&Component> {
        self.components.iter().find(|c| c.kind == kind)
    }

    /// Verify an on-disk artifact matches its manifest entry (size then digest).
    pub fn verify_artifact(&self, component: &Component, path: &std::path::Path) -> Result<()> {
        let (digest, size) = crate::digest::sha256_file(path)?;
        if size != component.artifact.size {
            return Err(Error::SizeMismatch {
                name: component.name.clone(),
                expected: component.artifact.size,
                actual: size,
            });
        }
        if digest != component.artifact.sha256 {
            return Err(Error::DigestMismatch {
                name: component.name.clone(),
                expected: component.artifact.sha256.clone(),
                actual: digest,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(min_app: Option<&str>) -> Component {
        Component {
            name: "os-imx8mm".into(),
            kind: ComponentKind::Os,
            version: "os-1".into(),
            artifact: Artifact {
                filename: "os.img".into(),
                sha256: "00".into(),
                size: 0,
            },
            min_app: min_app.map(Into::into),
        }
    }

    #[test]
    fn no_min_app_always_passes() {
        assert!(component(None).min_app_satisfied(None));
        assert!(component(None).min_app_satisfied(Some("garbage")));
    }

    #[test]
    fn min_app_orders_numerically_and_ignores_git_suffixes() {
        let c = component(Some("0.2.0"));
        assert!(c.min_app_satisfied(Some("0.2.0")));
        assert!(c.min_app_satisfied(Some("v0.2.1-g1234abc")));
        assert!(c.min_app_satisfied(Some("0.10.0"))); // numeric, not lexicographic
        assert!(!c.min_app_satisfied(Some("0.1.9-g1234abc")));
        // shorter prefix orders below a longer equal prefix
        assert!(!component(Some("0.2.0.1")).min_app_satisfied(Some("0.2.0")));
        assert!(component(Some("0.2")).min_app_satisfied(Some("0.2.0")));
    }

    #[test]
    fn min_app_fails_closed_on_unknown_or_non_numeric_versions() {
        let c = component(Some("0.2.0"));
        assert!(!c.min_app_satisfied(None));
        assert!(!c.min_app_satisfied(Some("nightly")));
        assert!(!c.min_app_satisfied(Some("1..2")));
        // an unparseable min_app also refuses rather than guessing
        assert!(!component(Some("release-7")).min_app_satisfied(Some("0.2.0")));
    }
}
