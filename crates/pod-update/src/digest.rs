//! SHA-256 helpers used for artifact integrity.

use crate::error::Result;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Hex-encoded SHA-256 of an in-memory byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Streaming SHA-256 of a file. Returns `(hex_digest, size_in_bytes)`.
pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex::encode(h.finalize()), total))
}
