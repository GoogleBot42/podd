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
