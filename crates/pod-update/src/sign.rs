//! Ed25519 signing and verification for update manifests.
//!
//! The release engineer holds a private key offline; the device bakes in one
//! or more trusted public keys and refuses any manifest that is not signed by
//! a trusted key. This is the antithesis of `curl | bash` / `git pull`.

use crate::error::{Error, Result};
use crate::manifest::Manifest;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use ed25519_dalek::{Signature, Signer, Verifier};
use serde::{Deserialize, Serialize};

// Re-export the key types so downstream crates need not depend on ed25519-dalek.
pub use ed25519_dalek::{SigningKey, VerifyingKey};

/// Short, stable identifier for a public key (first 16 hex of SHA-256 of the
/// key bytes). Lets a manifest name which key signed it, enabling rotation.
pub fn key_id(vk: &VerifyingKey) -> String {
    crate::digest::sha256_hex(vk.as_bytes())[..16].to_string()
}

/// Generate a fresh signing keypair from OS randomness.
pub fn generate_keypair() -> Result<(SigningKey, VerifyingKey)> {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|_| Error::Rng)?;
    let sk = SigningKey::from_bytes(&seed);
    let vk = sk.verifying_key();
    Ok((sk, vk))
}

/// Encode a signing (private) key as base64 of its 32-byte seed.
pub fn encode_signing_key(sk: &SigningKey) -> String {
    B64.encode(sk.to_bytes())
}

/// Encode a verifying (public) key as base64 of its 32 bytes.
pub fn encode_verifying_key(vk: &VerifyingKey) -> String {
    B64.encode(vk.as_bytes())
}

/// Parse a base64-encoded signing key (32-byte seed).
pub fn decode_signing_key(s: &str) -> Result<SigningKey> {
    let bytes = B64.decode(s.trim())?;
    let seed: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::BadKey("signing key must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Parse a base64-encoded verifying key (32 bytes).
pub fn decode_verifying_key(s: &str) -> Result<VerifyingKey> {
    let bytes = B64.decode(s.trim())?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::BadKey("verifying key must be 32 bytes"))?;
    VerifyingKey::from_bytes(&arr).map_err(|_| Error::BadKey("invalid verifying key"))
}

/// A manifest plus a detached signature over its canonical bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedManifest {
    pub manifest: Manifest,
    /// key_id of the key that produced `signature`.
    pub key_id: String,
    /// base64 of the 64-byte Ed25519 signature.
    pub signature: String,
}

impl SignedManifest {
    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }
}

/// Sign a manifest, producing a [`SignedManifest`].
pub fn sign_manifest(manifest: &Manifest, sk: &SigningKey) -> Result<SignedManifest> {
    let bytes = manifest.canonical_bytes()?;
    let sig = sk.sign(&bytes);
    Ok(SignedManifest {
        manifest: manifest.clone(),
        key_id: key_id(&sk.verifying_key()),
        signature: B64.encode(sig.to_bytes()),
    })
}

/// Verify a [`SignedManifest`] against a set of trusted public keys.
/// Returns the verified [`Manifest`] on success.
pub fn verify_manifest(sm: &SignedManifest, trusted: &[VerifyingKey]) -> Result<Manifest> {
    let bytes = sm.manifest.canonical_bytes()?;
    let sig_bytes = B64.decode(&sm.signature)?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::BadSignature)?;
    let sig = Signature::from_bytes(&sig_arr);

    for vk in trusted {
        if key_id(vk) == sm.key_id && vk.verify(&bytes, &sig).is_ok() {
            return Ok(sm.manifest.clone());
        }
    }
    Err(Error::SignatureInvalid(sm.key_id.clone()))
}
