// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Bearer token generation and secure storage.
//!
//! # Security model
//!
//! Raw tokens are **never persisted**. They are shown to the operator once at
//! generation time, then immediately hashed. Only the SHA-256 digest is stored
//! on disk. If the token file is leaked, an attacker learns a hash — they still
//! need to invert SHA-256 over a 256-bit key space, which is infeasible.
//!
//! All comparisons use [`subtle::ConstantTimeEq`] to prevent timing oracles.
//!
//! # Usage
//!
//! ```rust
//! use sven_node::crypto::token::{RawToken, StoredTokenFile};
//!
//! // Generate and display a token once.
//! let raw = RawToken::generate();
//! println!("Save this token: {}", raw.as_str());
//!
//! // Hash it for storage — the raw value is consumed.
//! let stored = raw.into_stored();
//!
//! // Verify an incoming bearer token in constant time.
//! assert!(stored.verify("the-right-token") == false); // wrong token
//! ```
//!
//! To generate **and** write to disk atomically (mode 0o600):
//!
//! ```rust,no_run
//! # use sven_node::crypto::token::StoredTokenFile;
//! # use std::path::Path;
//! let raw = StoredTokenFile::generate_and_save(Path::new("/tmp/token.yaml")).unwrap();
//! println!("New token (save it!): {}", raw.as_str());
//! // Later, load and verify:
//! let file = StoredTokenFile::load(Path::new("/tmp/token.yaml")).unwrap();
//! assert!(file.token_hash.verify(raw.as_str()));
//! ```

use std::path::Path;

use anyhow::Context;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// A raw bearer token — displayed to the operator **exactly once**.
///
/// Call [`RawToken::into_stored`] immediately after displaying it; then drop
/// this value so it never reaches persistent storage.
#[derive(Debug)]
#[must_use = "display this token to the operator, then call into_stored()"]
pub struct RawToken(String);

impl RawToken {
    /// Generate a cryptographically random 256-bit token.
    ///
    /// Uses the OS CSPRNG (`/dev/urandom` on Linux, `BCryptGenRandom` on
    /// Windows). The returned string is 43 characters of base64url encoding.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        RawToken(base64url_encode(&bytes))
    }

    /// The raw token string. Show this to the operator exactly once.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Hash the token and discard the plaintext.
    pub fn into_stored(self) -> StoredToken {
        StoredToken(sha256(self.0.as_bytes()))
    }
}

impl std::fmt::Display for RawToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The stored form of a bearer token — only the SHA-256 digest is persisted.
///
/// Deserialised from/serialised to YAML. Never contains the raw token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredToken(#[serde(with = "hex_bytes")] [u8; 32]);

impl StoredToken {
    /// Verify a provided token string in constant time.
    ///
    /// Returns `true` iff `SHA-256(provided) == self.0`.
    pub fn verify(&self, provided: &str) -> bool {
        let provided_hash = sha256(provided.as_bytes());
        bool::from(provided_hash.ct_eq(&self.0))
    }

    /// Construct from a known hex-encoded digest (for tests).
    #[cfg(test)]
    pub fn from_hex(hex_str: &str) -> anyhow::Result<Self> {
        let bytes = hex::decode(hex_str)?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("wrong length"))?;
        Ok(StoredToken(arr))
    }
}

/// On-disk YAML format for the token file.
///
/// Example `~/.config/sven/gateway/token.yaml`:
/// ```yaml
/// # SHA-256 hash of the bearer token. The raw token was shown once at generation.
/// token_hash: "a3f2...b7"
/// ```
#[derive(Debug, Serialize, Deserialize)]
pub struct StoredTokenFile {
    /// Hex-encoded SHA-256 digest of the bearer token.
    pub token_hash: StoredToken,
}

impl StoredTokenFile {
    /// Generate a new token, write the hash to `path`, and return the raw
    /// token so the caller can display it once.
    pub fn generate_and_save(path: &Path) -> anyhow::Result<RawToken> {
        // Keep the raw string before consuming via into_stored.
        let raw = RawToken::generate();
        let raw_str = raw.as_str().to_string();
        let stored = raw.into_stored();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating token directory {}", parent.display()))?;
        }

        let file = StoredTokenFile { token_hash: stored };
        let yaml = serde_yaml::to_string(&file).context("serializing token file")?;

        write_secret_file(path, yaml.as_bytes())?;

        // Return a RawToken wrapping the original string so the caller can
        // display it exactly once.
        Ok(RawToken(raw_str))
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading token file {}", path.display()))?;
        serde_yaml::from_str(&text)
            .with_context(|| format!("parsing token file {}", path.display()))
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Write `data` to `path` with mode 0o600 on Unix (owner-read/write only).
fn write_secret_file(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("writing secret file {}", path.display()))?;
        f.write_all(data)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
            .with_context(|| format!("writing secret file {}", path.display()))?;
    }
    Ok(())
}

fn base64url_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Serde helper: serialize/deserialize a `[u8; 32]` as a lowercase hex string.
mod hex_bytes {
    use serde::{de::Error, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = hex::decode(&s).map_err(D::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| D::Error::custom("expected 32-byte hex"))
    }

    use serde::Deserialize;
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_token_is_43_chars() {
        let t = RawToken::generate();
        // base64url of 32 bytes = ceil(32 * 4/3) = 43 chars (no padding)
        assert_eq!(t.as_str().len(), 43, "token must be 43 base64url chars");
    }

    #[test]
    fn stored_token_verifies_correct_raw() {
        let raw = RawToken::generate();
        let raw_str = raw.as_str().to_string();
        let stored = raw.into_stored();
        assert!(
            stored.verify(&raw_str),
            "stored token must verify the original raw token"
        );
    }

    #[test]
    fn stored_token_rejects_wrong_value() {
        let raw = RawToken::generate();
        let stored = raw.into_stored();
        assert!(!stored.verify("wrong-token"), "must reject wrong token");
    }

    #[test]
    fn stored_token_rejects_empty_string() {
        let raw = RawToken::generate();
        let stored = raw.into_stored();
        assert!(!stored.verify(""));
    }

    #[test]
    fn two_generated_tokens_are_different() {
        let t1 = RawToken::generate().into_stored();
        let t2 = RawToken::generate().into_stored();
        // The probability of collision is 2^{-256}: effectively zero.
        assert_ne!(format!("{:?}", t1.0), format!("{:?}", t2.0));
    }

    #[test]
    fn stored_token_yaml_round_trip() {
        let raw = RawToken::generate();
        let raw_str = raw.as_str().to_string();
        let stored = raw.into_stored();
        let file = StoredTokenFile { token_hash: stored };
        let yaml = serde_yaml::to_string(&file).unwrap();
        let back: StoredTokenFile = serde_yaml::from_str(&yaml).unwrap();
        assert!(
            back.token_hash.verify(&raw_str),
            "round-tripped token must verify"
        );
    }

    #[test]
    fn token_file_generate_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.yaml");
        let raw = StoredTokenFile::generate_and_save(&path).unwrap();
        let raw_str = raw.as_str().to_string();
        let loaded = StoredTokenFile::load(&path).unwrap();
        assert!(loaded.token_hash.verify(&raw_str));
    }
}
