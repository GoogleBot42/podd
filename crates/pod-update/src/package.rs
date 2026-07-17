//! Build release artifacts. App/OS payloads are packed as **reproducible**
//! read-only squashfs images: the device mounts them read-only under a
//! versioned path and flips a `current` symlink, so a half-applied update can
//! never leave a mutable, partially-written directory.

use crate::error::{Error, Result};
use crate::manifest::Artifact;
use std::path::Path;
use std::process::Command;

/// Pack `src_dir` into a reproducible squashfs at `out_file`, returning the
/// [`Artifact`] (filename + digest + size) for the manifest.
///
/// Reproducibility flags pin ownership and timestamps so identical inputs
/// yield byte-identical images (and therefore identical digests).
pub fn pack_squashfs(src_dir: &Path, out_file: &Path) -> Result<Artifact> {
    if out_file.exists() {
        std::fs::remove_file(out_file)?;
    }
    let output = Command::new("mksquashfs")
        .arg(src_dir)
        .arg(out_file)
        .args([
            "-all-root",      // uid/gid 0, independent of build host
            "-mkfs-time", "0",
            "-all-time", "0", // pin all mtimes
            "-no-xattrs",
            "-noappend",
            "-reproducible",
            "-comp", "zstd",
            "-no-progress",
        ])
        .output()
        .map_err(|e| Error::Pack(format!("failed to spawn mksquashfs: {e}")))?;

    if !output.status.success() {
        return Err(Error::Pack(format!(
            "mksquashfs exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let (sha256, size) = crate::digest::sha256_file(out_file)?;
    let filename = out_file
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Error::Pack("output path has no filename".into()))?
        .to_string();
    Ok(Artifact {
        filename,
        sha256,
        size,
    })
}

/// Compute an [`Artifact`] for a file that is already built (e.g. an MCU
/// `.bbin` blob or a RAUC bundle produced elsewhere).
pub fn artifact_for_file(path: &Path) -> Result<Artifact> {
    let (sha256, size) = crate::digest::sha256_file(path)?;
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Error::Pack("path has no filename".into()))?
        .to_string();
    Ok(Artifact {
        filename,
        sha256,
        size,
    })
}
