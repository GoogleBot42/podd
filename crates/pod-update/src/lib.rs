//! `pod-update` — the signed, reproducible update core shared by the `podup`
//! release tool (host side) and the `podd` update agent (device side).
//!
//! Design goals (see `REPLACEMENT_PLAN.md` §9):
//! - **Verified**: every manifest is Ed25519-signed; the device refuses
//!   anything not signed by a trusted key.
//! - **Reproducible**: artifacts are content-addressed (SHA-256) and built
//!   deterministically; a version is a content hash, and nothing is built on
//!   the device.
//! - **Atomic + coherent**: a manifest describes a whole release; components
//!   that must match ship together.

pub mod digest;
pub mod error;
pub mod manifest;
pub mod package;
pub mod sign;

pub use error::{Error, Result};
pub use manifest::{Artifact, Component, ComponentKind, Manifest, SCHEMA_VERSION};
pub use sign::{
    generate_keypair, sign_manifest, verify_release, verify_signature, SignedManifest, TrustPolicy,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> Manifest {
        let mut m = Manifest::new("stable", 1_700_000_000);
        m.components.push(Component {
            name: "podd".into(),
            kind: ComponentKind::App,
            version: "0.0.1+abc123".into(),
            artifact: Artifact {
                filename: "podd-0.0.1.squashfs".into(),
                sha256: digest::sha256_hex(b"hello world"),
                size: 11,
            },
            min_app: None,
        });
        m
    }

    #[test]
    fn canonical_bytes_are_stable() {
        let m = sample_manifest();
        assert_eq!(m.canonical_bytes().unwrap(), m.canonical_bytes().unwrap());
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let (sk, vk) = generate_keypair().unwrap();
        let m = sample_manifest();
        let signed = sign_manifest(&m, &sk).unwrap();
        let verified = verify_release(&signed, &TrustPolicy::RequireSigned(vec![vk])).unwrap();
        assert_eq!(verified, m);
    }

    #[test]
    fn tampered_manifest_fails_verification() {
        let (sk, vk) = generate_keypair().unwrap();
        let m = sample_manifest();
        let mut signed = sign_manifest(&m, &sk).unwrap();
        // Flip a version string after signing.
        signed.manifest.components[0].version = "9.9.9".into();
        assert!(verify_release(&signed, &TrustPolicy::RequireSigned(vec![vk])).is_err());
    }

    #[test]
    fn untrusted_key_fails_verification() {
        let (sk, _vk) = generate_keypair().unwrap();
        let (_sk2, vk2) = generate_keypair().unwrap();
        let signed = sign_manifest(&sample_manifest(), &sk).unwrap();
        // Verifying against a *different* key must fail.
        assert!(verify_release(&signed, &TrustPolicy::RequireSigned(vec![vk2])).is_err());
    }

    #[test]
    fn unsigned_accepted_only_when_policy_allows() {
        let m = sample_manifest();
        let unsigned = SignedManifest::unsigned(m.clone());
        assert!(!unsigned.is_signed());
        // Owner opted into unsigned → accepted.
        assert_eq!(
            verify_release(&unsigned, &TrustPolicy::AllowUnsigned).unwrap(),
            m
        );
        // Owner requires a signature → unsigned rejected.
        let (_sk, vk) = generate_keypair().unwrap();
        assert!(matches!(
            verify_release(&unsigned, &TrustPolicy::RequireSigned(vec![vk])),
            Err(Error::SignatureRequired)
        ));
    }

    #[test]
    fn signed_manifest_also_accepted_under_allow_unsigned() {
        let (sk, _vk) = generate_keypair().unwrap();
        let signed = sign_manifest(&sample_manifest(), &sk).unwrap();
        // AllowUnsigned accepts signed manifests too (owner opted out of auth).
        assert!(verify_release(&signed, &TrustPolicy::AllowUnsigned).is_ok());
    }

    #[test]
    fn key_encoding_roundtrip() {
        let (sk, vk) = generate_keypair().unwrap();
        let sk2 = sign::decode_signing_key(&sign::encode_signing_key(&sk)).unwrap();
        let vk2 = sign::decode_verifying_key(&sign::encode_verifying_key(&vk)).unwrap();
        assert_eq!(sk.to_bytes(), sk2.to_bytes());
        assert_eq!(vk.as_bytes(), vk2.as_bytes());
    }

    #[test]
    fn verify_artifact_detects_corruption() {
        let dir = std::env::temp_dir().join(format!("podup-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("blob.bin");
        std::fs::write(&f, b"the real bytes").unwrap();

        let good = Artifact {
            filename: "blob.bin".into(),
            sha256: digest::sha256_hex(b"the real bytes"),
            size: 14,
        };
        let comp = Component {
            name: "blob".into(),
            kind: ComponentKind::McuFrozen,
            version: "1".into(),
            artifact: good,
            min_app: None,
        };
        let mut m = Manifest::new("stable", 0);
        m.components.push(comp.clone());
        assert!(m.verify_artifact(&comp, &f).is_ok());

        // Corrupt the file; digest check must fail.
        std::fs::write(&f, b"the fake bytes").unwrap();
        assert!(matches!(
            m.verify_artifact(&comp, &f),
            Err(Error::DigestMismatch { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
