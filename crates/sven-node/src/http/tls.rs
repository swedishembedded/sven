// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! TLS certificate management — pure Rust, no OpenSSL.
//!
//! # Choices vs. openclaw
//!
//! | Property         | openclaw              | sven-node              |
//! |------------------|-----------------------|---------------------------|
//! | Key algorithm    | RSA 2048              | ECDSA P-256 (smaller+faster) |
//! | Cert validity    | 10 years              | 90 days (like Let's Encrypt) |
//! | Generation       | `openssl` subprocess  | `rcgen` (pure Rust, no PATH dep) |
//! | TLS minimum      | Optional, TLS 1.2     | Mandatory, TLS 1.3        |
//!
//! # Auto-rotation
//!
//! Certificates are regenerated when they expire (within 7 days of expiry).
//! The gateway reloads the cert file on each restart; for zero-downtime
//! rotation, restart the gateway after the cert is regenerated.
//!
//! # Client pinning
//!
//! The SHA-256 fingerprint of the generated cert is printed at startup.
//! Native clients (mobile, CLI) should pin this fingerprint and verify it on
//! reconnect — identical to how SSH handles host keys (TOFU).

use std::path::{Path, PathBuf};

use anyhow::Context;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rustls_pemfile::certs;
use time::{Duration, OffsetDateTime};
use tracing::info;

/// Certificate validity window: 90 days.
const CERT_VALIDITY_DAYS: i64 = 90;

/// Regenerate the cert this many days before it expires.
const CERT_RENEW_BEFORE_DAYS: i64 = 7;

/// Loaded TLS configuration — fingerprint only; ServerConfig is built via
/// [`load_rustls_config`] to avoid rustls version conflicts.
pub struct TlsRuntime {
    pub cert_path: std::path::PathBuf,
    pub key_path: std::path::PathBuf,
    /// Hex-encoded SHA-256 fingerprint for display / client pinning.
    pub fingerprint_sha256: String,
}

/// Load or (re-)generate the gateway's TLS certificate.
///
/// The certificate and key are stored in `cert_dir`:
/// - `gateway-cert.pem`
/// - `gateway-key.pem`
///
/// If either file is missing or the cert is near expiry, a new ECDSA P-256
/// self-signed certificate is generated in pure Rust (no subprocess).
pub fn load_or_generate(cert_dir: &Path) -> anyhow::Result<TlsRuntime> {
    let cert_path = cert_dir.join("gateway-cert.pem");
    let key_path = cert_dir.join("gateway-key.pem");

    let needs_generate =
        !cert_path.exists() || !key_path.exists() || cert_is_expiring_soon(&cert_path);

    if needs_generate {
        generate_self_signed(cert_dir, &cert_path, &key_path)?;
    }

    load_from_files(&cert_path, &key_path)
}

// ── Certificate generation ────────────────────────────────────────────────────

fn generate_self_signed(cert_dir: &Path, cert_path: &Path, key_path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(cert_dir)
        .with_context(|| format!("creating TLS cert dir {}", cert_dir.display()))?;

    // ECDSA P-256: equivalent security to RSA 2048, 3× smaller keys.
    // rcgen 0.13: KeyPair::generate() defaults to ECDSA P-256.
    let key_pair = KeyPair::generate().context("generating ECDSA P-256 key pair")?;

    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "sven-node");

    let now = OffsetDateTime::now_utc();
    let mut params = CertificateParams::new(vec![
        "sven-node".to_string(),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ])
    .context("building cert params")?;
    params.not_before = now;
    // 90 days: matches Let's Encrypt cadence, limits exposure window.
    params.not_after = now + Duration::days(CERT_VALIDITY_DAYS);
    params.distinguished_name = dn;

    // rcgen 0.13 API: params.self_signed(&key_pair) signs the cert.
    let cert = params
        .self_signed(&key_pair)
        .context("generating self-signed certificate")?;

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    // Write cert: readable, no sensitive data.
    std::fs::write(cert_path, &cert_pem)
        .with_context(|| format!("writing {}", cert_path.display()))?;

    // Write key with mode 0o600 — owner read/write only.
    write_secret(key_path, key_pem.as_bytes())?;

    info!(
        cert = %cert_path.display(),
        key  = %key_path.display(),
        "generated ECDSA P-256 self-signed certificate (90-day validity)",
    );

    Ok(())
}

// ── Certificate loading ───────────────────────────────────────────────────────

fn load_from_files(cert_path: &Path, key_path: &Path) -> anyhow::Result<TlsRuntime> {
    let cert_pem =
        std::fs::read(cert_path).with_context(|| format!("reading {}", cert_path.display()))?;

    // Compute SHA-256 fingerprint of the first certificate for display / client pinning.
    let fingerprint_sha256 = {
        use sha2::{Digest, Sha256};
        let mut reader = std::io::Cursor::new(&cert_pem);
        let first_cert = certs(&mut reader)
            .next()
            .ok_or_else(|| anyhow::anyhow!("no certificate found in {}", cert_path.display()))?
            .context("parsing certificate")?;
        let digest = Sha256::digest(&first_cert);
        digest
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    };

    info!(fingerprint = %fingerprint_sha256, "loaded TLS certificate");

    Ok(TlsRuntime {
        cert_path: cert_path.to_path_buf(),
        key_path: key_path.to_path_buf(),
        fingerprint_sha256,
    })
}

// ── Expiry check ──────────────────────────────────────────────────────────────

fn cert_is_expiring_soon(cert_path: &Path) -> bool {
    let Ok(pem) = std::fs::read(cert_path) else {
        return true;
    };
    let mut reader = std::io::Cursor::new(&pem);
    let Ok(Some(cert_der)) = certs(&mut reader).next().transpose() else {
        return true;
    };

    // Parse the X.509 certificate to get the notAfter field.
    // We use the raw DER bytes; for a quick expiry check we look for the
    // validity period. Use rustls's built-in x509 parser if available,
    // otherwise fall back to re-generating (safe: generates a fresh cert).
    //
    // For simplicity: if the file is older than (validity - renew_before),
    // treat it as expiring. ECDSA P-256 certs are cheap to regenerate.
    let Ok(meta) = std::fs::metadata(cert_path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    let age_days = modified.elapsed().unwrap_or_default().as_secs() as i64 / 86400;

    let _ = cert_der; // suppress unused warning
    age_days >= (CERT_VALIDITY_DAYS - CERT_RENEW_BEFORE_DAYS)
}

// ── Secret file helper ────────────────────────────────────────────────────────

fn write_secret(path: &Path, data: &[u8]) -> anyhow::Result<()> {
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
            .with_context(|| format!("writing {}", path.display()))?;
        f.write_all(data)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

// ── Convenience: default cert dir ─────────────────────────────────────────────

pub fn default_cert_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/sven/gateway/tls")
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_cert_and_loads_it() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = load_or_generate(dir.path()).unwrap();
        assert!(!runtime.fingerprint_sha256.is_empty());
        assert!(runtime.fingerprint_sha256.contains(':'));
    }

    #[test]
    fn cert_files_are_created() {
        let dir = tempfile::tempdir().unwrap();
        load_or_generate(dir.path()).unwrap();
        assert!(dir.path().join("gateway-cert.pem").exists());
        assert!(dir.path().join("gateway-key.pem").exists());
    }

    #[test]
    fn second_load_reuses_existing_cert() {
        let dir = tempfile::tempdir().unwrap();
        let r1 = load_or_generate(dir.path()).unwrap();
        let r2 = load_or_generate(dir.path()).unwrap();
        // Same cert → same fingerprint.
        assert_eq!(r1.fingerprint_sha256, r2.fingerprint_sha256);
    }

    #[test]
    #[cfg(unix)]
    fn key_file_has_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load_or_generate(dir.path()).unwrap();
        let meta = std::fs::metadata(dir.path().join("gateway-key.pem")).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file must be 0600, got {mode:03o}");
    }
}
