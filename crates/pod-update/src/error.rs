use thiserror::Error;

/// Errors produced when building, signing, or verifying updates.
#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("hex decode error: {0}")]
    Hex(#[from] hex::FromHexError),

    #[error("could not gather randomness")]
    Rng,

    #[error("key material is malformed: {0}")]
    BadKey(&'static str),

    #[error("signature is malformed")]
    BadSignature,

    #[error("no trusted key produced a valid signature (key_id={0:?})")]
    SignatureInvalid(Option<String>),

    #[error("policy requires a signature but the manifest is unsigned")]
    SignatureRequired,

    #[error("artifact size mismatch for {name}: manifest={expected} actual={actual}")]
    SizeMismatch {
        name: String,
        expected: u64,
        actual: u64,
    },

    #[error("artifact digest mismatch for {name}: manifest={expected} actual={actual}")]
    DigestMismatch {
        name: String,
        expected: String,
        actual: String,
    },

    #[error("packaging tool failed: {0}")]
    Pack(String),
}

pub type Result<T> = std::result::Result<T, Error>;
