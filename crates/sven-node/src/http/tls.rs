// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! TLS certificate provisioning — pure Rust, no OpenSSL.
//!
//! # Modes
//!
//! | Mode          | Trust               | Setup               |
//! |---------------|---------------------|---------------------|
//! | `auto`        | Tailscale → LocalCA | Zero (Tailscale) or once (LocalCA) |
//! | `tailscale`   | Browser-trusted     | Zero (needs Tailscale) |
//! | `local-ca`    | CA trust once       | `sven node install-ca` once per device |
//! | `self-signed` | Fingerprint pinning | Accept exception per browser |
//! | `files`       | Whatever you bring  | BYOC |
//!
//! # Local CA trust chain
//!
//! ```text
//! ca-key.pem ──► ca-cert.pem   (10yr, stored in tls_cert_dir)
//!                    │
//!                    ▼ signs
//!              gateway-cert.pem (90d, contains all SANs)
//! ```
//!
//! Users install `ca-cert.pem` once via `sven node install-ca`. Every
//! subsequent 90-day rotation is transparent — no browser interaction needed.
//!
//! # Tailscale
//!
//! When `tls_mode: tailscale` (or `auto` with Tailscale detected):
//!
//! ```text
//! tailscale status --json → DNSName → machine.tailnet.ts.net
//! tailscale cert --cert-file … --key-file … machine.tailnet.ts.net
//! ```
//!
//! The resulting cert is issued by Let's Encrypt and trusted everywhere.
//! Tailscale renews it automatically; sven picks up the new cert on restart.

use std::path::{Path, PathBuf};

use anyhow::Context;
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use rustls_pemfile::certs;
use time::{Duration, OffsetDateTime};
use tracing::{info, warn};

use crate::config::TlsMode;

/// Certificate validity window for server certs: 90 days.
const CERT_VALIDITY_DAYS: i64 = 90;
/// Regenerate the server cert this many days before expiry.
const CERT_RENEW_BEFORE_DAYS: i64 = 7;
/// CA cert validity: 10 years (re-trust is not needed on rotation).
const CA_VALIDITY_DAYS: i64 = 365 * 10;

/// Loaded TLS runtime — paths + fingerprint for display / client pinning.
pub struct TlsRuntime {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    /// Colon-separated hex SHA-256 fingerprint for display / TOFU pinning.
    pub fingerprint_sha256: String,
    /// Provisioning mode actually used (may differ from config in `auto` mode).
    pub mode_used: TlsModeUsed,
}

/// Which provisioning path was actually taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsModeUsed {
    Tailscale { fqdn: String },
    LocalCa,
    SelfSigned,
    Files,
}

/// Provision TLS certificates according to `mode`.
///
/// - `cert_dir`: directory to store / load certs (created if needed).
/// - `bind_addr`: the configured `http.bind` value; if it's a specific IP,
///   it's added to the server cert's SANs automatically.
/// - `san_extra`: additional hostnames/IPs to include in SANs.
pub fn provision(
    mode: &TlsMode,
    cert_dir: &Path,
    bind_addr: &str,
    san_extra: &[String],
) -> anyhow::Result<TlsRuntime> {
    match mode {
        TlsMode::Auto => {
            // Try Tailscale first; fall back to local CA.
            if let Some(fqdn) = tailscale_fqdn() {
                match tailscale_provision(cert_dir, &fqdn) {
                    Ok(r) => return Ok(r),
                    Err(e) => {
                        warn!("Tailscale cert failed ({e}), falling back to local-ca");
                    }
                }
            }
            local_ca_provision(cert_dir, bind_addr, san_extra)
        }
        TlsMode::Tailscale => {
            let fqdn = tailscale_fqdn().ok_or_else(|| {
                anyhow::anyhow!(
                    "tls_mode: tailscale — but `tailscale status` failed or Tailscale is not \
                     running. Install Tailscale and join a tailnet, or change tls_mode to \
                     local-ca."
                )
            })?;
            tailscale_provision(cert_dir, &fqdn)
        }
        TlsMode::LocalCa => local_ca_provision(cert_dir, bind_addr, san_extra),
        TlsMode::SelfSigned => self_signed_provision(cert_dir, bind_addr, san_extra),
        TlsMode::Files => {
            let cert_path = cert_dir.join("gateway-cert.pem");
            let key_path = cert_dir.join("gateway-key.pem");
            load_from_files(&cert_path, &key_path, TlsModeUsed::Files)
        }
    }
}

// ── Tailscale provisioning ────────────────────────────────────────────────────

/// Detect Tailscale and return the machine's FQDN (e.g. `mybox.ts.net`).
fn tailscale_fqdn() -> Option<String> {
    let out = std::process::Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    // DNSName is a fully-qualified name with trailing dot: "mybox.tailnet.ts.net."
    let fqdn = json["Self"]["DNSName"]
        .as_str()?
        .trim_end_matches('.')
        .to_string();
    if fqdn.is_empty() {
        return None;
    }
    Some(fqdn)
}

/// Call `tailscale cert` to fetch/renew a Let's Encrypt cert for `fqdn`.
fn tailscale_provision(cert_dir: &Path, fqdn: &str) -> anyhow::Result<TlsRuntime> {
    std::fs::create_dir_all(cert_dir)
        .with_context(|| format!("creating TLS cert dir {}", cert_dir.display()))?;

    let cert_path = cert_dir.join("tailscale-cert.pem");
    let key_path = cert_dir.join("tailscale-key.pem");

    let status = std::process::Command::new("tailscale")
        .args([
            "cert",
            "--cert-file",
            cert_path.to_str().context("cert path is not valid UTF-8")?,
            "--key-file",
            key_path.to_str().context("key path is not valid UTF-8")?,
            fqdn,
        ])
        .status()
        .context("running `tailscale cert`")?;

    anyhow::ensure!(
        status.success(),
        "`tailscale cert` exited with status {status}"
    );

    // Restrict key file permissions.
    restrict_file_permissions(&key_path)?;

    info!(
        fqdn,
        "using Tailscale Let's Encrypt certificate (browser-trusted)"
    );
    load_from_files(
        &cert_path,
        &key_path,
        TlsModeUsed::Tailscale {
            fqdn: fqdn.to_string(),
        },
    )
}

// ── Local CA provisioning ─────────────────────────────────────────────────────

/// Generate (or load) a local CA and issue a server cert signed by it.
///
/// On first run the CA cert is printed with instructions to trust it.
/// On subsequent runs the CA is silently reused; the server cert is
/// regenerated when it approaches expiry.
fn local_ca_provision(
    cert_dir: &Path,
    bind_addr: &str,
    san_extra: &[String],
) -> anyhow::Result<TlsRuntime> {
    std::fs::create_dir_all(cert_dir)
        .with_context(|| format!("creating TLS cert dir {}", cert_dir.display()))?;

    let ca_cert_path = cert_dir.join("ca-cert.pem");
    let ca_key_path = cert_dir.join("ca-key.pem");
    let server_cert_path = cert_dir.join("gateway-cert.pem");
    let server_key_path = cert_dir.join("gateway-key.pem");

    let first_run = !ca_cert_path.exists() || !ca_key_path.exists();

    // ── Load or generate CA ───────────────────────────────────────────────────
    let (ca_cert_obj, ca_key) = load_or_generate_ca(&ca_cert_path, &ca_key_path)?;

    if first_run {
        print_ca_trust_instructions(&ca_cert_path);
    }

    // ── Load or generate server cert ─────────────────────────────────────────
    let needs_server_cert = !server_cert_path.exists()
        || !server_key_path.exists()
        || cert_is_expiring_soon(&server_cert_path);

    if needs_server_cert {
        let sans = build_sans(bind_addr, san_extra);
        generate_server_cert(
            &ca_cert_obj,
            &ca_key,
            &server_cert_path,
            &server_key_path,
            sans,
        )?;
    }

    let runtime = load_from_files(&server_cert_path, &server_key_path, TlsModeUsed::LocalCa)?;
    if first_run {
        info!(
            ca_cert = %ca_cert_path.display(),
            "TLS: local CA — run `sven node install-ca` to make browsers trust this node",
        );
    }
    Ok(runtime)
}

/// Load existing CA from disk or generate a new one.
///
/// When an existing CA key is found, the CA `Certificate` object is
/// reconstructed in-memory from the same fixed DN and the stored key.
/// The public key is identical, so:
/// - SKI (Subject Key Identifier) in the CA cert matches the stored cert.
/// - AKI (Authority Key Identifier) in signed server certs resolves to the
///   stored CA cert in the user's trust store.
/// - Server cert signatures verify against the stored CA cert's public key.
/// The in-memory cert is only used for signing; users keep the stored cert.
fn load_or_generate_ca(
    ca_cert_path: &Path,
    ca_key_path: &Path,
) -> anyhow::Result<(rcgen::Certificate, KeyPair)> {
    if ca_key_path.exists() {
        let ca_key_pem = std::fs::read_to_string(ca_key_path).context("reading CA key")?;
        let ca_key = KeyPair::from_pem(&ca_key_pem).context("parsing CA key")?;
        // Reconstruct a signing-capable CA cert from the same key + fixed DN.
        let ca_cert = build_ca_cert_in_memory(&ca_key)?;
        return Ok((ca_cert, ca_key));
    }

    // First run: generate a new CA.
    let ca_key = KeyPair::generate().context("generating CA key pair")?;
    let ca_cert = build_ca_cert_in_memory(&ca_key)?;

    // Write CA cert (world-readable: users need to import it).
    std::fs::write(ca_cert_path, ca_cert.pem())
        .with_context(|| format!("writing {}", ca_cert_path.display()))?;
    // Write CA key (private: 0o600).
    write_secret(ca_key_path, ca_key.serialize_pem().as_bytes())?;

    info!(
        ca_cert = %ca_cert_path.display(),
        "generated new local CA certificate (10-year validity)",
    );

    Ok((ca_cert, ca_key))
}

/// Build an in-memory CA `Certificate` for signing.
///
/// Uses a fixed DN so that issuer fields in server certs always match the
/// stored `ca-cert.pem` regardless of when the CA cert object is rebuilt.
fn build_ca_cert_in_memory(ca_key: &KeyPair) -> anyhow::Result<rcgen::Certificate> {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Sven Local CA");
    dn.push(DnType::OrganizationName, "Sven Node");

    let now = OffsetDateTime::now_utc();
    let mut params = CertificateParams::new(vec![]).context("building CA params")?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.distinguished_name = dn;
    // Start 1 day in the past to avoid "not yet valid" edge cases.
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(CA_VALIDITY_DAYS);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];

    params
        .self_signed(ca_key)
        .context("building CA certificate")
}

/// Sign a new 90-day server cert with the CA.
fn generate_server_cert(
    ca_cert: &rcgen::Certificate,
    ca_key: &KeyPair,
    cert_path: &Path,
    key_path: &Path,
    sans: Vec<String>,
) -> anyhow::Result<()> {
    let server_key = KeyPair::generate().context("generating server key")?;

    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "sven-node");

    let now = OffsetDateTime::now_utc();
    let mut params = CertificateParams::new(sans.clone()).context("building server cert params")?;
    params.distinguished_name = dn;
    params.not_before = now;
    params.not_after = now + Duration::days(CERT_VALIDITY_DAYS);

    let cert = params
        .signed_by(&server_key, ca_cert, ca_key)
        .context("signing server cert with CA")?;

    std::fs::write(cert_path, cert.pem())
        .with_context(|| format!("writing {}", cert_path.display()))?;
    write_secret(key_path, server_key.serialize_pem().as_bytes())?;

    info!(
        cert = %cert_path.display(),
        sans = %sans.join(", "),
        "issued CA-signed server certificate (90-day validity)",
    );
    Ok(())
}

// ── Self-signed provisioning (legacy / fallback) ──────────────────────────────

fn self_signed_provision(
    cert_dir: &Path,
    bind_addr: &str,
    san_extra: &[String],
) -> anyhow::Result<TlsRuntime> {
    std::fs::create_dir_all(cert_dir)
        .with_context(|| format!("creating TLS cert dir {}", cert_dir.display()))?;

    let cert_path = cert_dir.join("gateway-cert.pem");
    let key_path = cert_dir.join("gateway-key.pem");

    let needs_generate =
        !cert_path.exists() || !key_path.exists() || cert_is_expiring_soon(&cert_path);

    if needs_generate {
        let sans = build_sans(bind_addr, san_extra);
        generate_self_signed_cert(cert_dir, &cert_path, &key_path, sans)?;
    }

    load_from_files(&cert_path, &key_path, TlsModeUsed::SelfSigned)
}

fn generate_self_signed_cert(
    _cert_dir: &Path,
    cert_path: &Path,
    key_path: &Path,
    sans: Vec<String>,
) -> anyhow::Result<()> {
    let key_pair = KeyPair::generate().context("generating ECDSA P-256 key pair")?;

    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "sven-node");

    let now = OffsetDateTime::now_utc();
    let mut params = CertificateParams::new(sans.clone()).context("building cert params")?;
    params.not_before = now;
    params.not_after = now + Duration::days(CERT_VALIDITY_DAYS);
    params.distinguished_name = dn;

    let cert = params
        .self_signed(&key_pair)
        .context("generating self-signed certificate")?;

    std::fs::write(cert_path, cert.pem())
        .with_context(|| format!("writing {}", cert_path.display()))?;
    write_secret(key_path, key_pair.serialize_pem().as_bytes())?;

    info!(
        cert = %cert_path.display(),
        sans = %sans.join(", "),
        "generated ECDSA P-256 self-signed certificate (90-day validity)",
    );
    Ok(())
}

// ── Compatibility shim for callers that used the old load_or_generate() ───────

/// Load or generate a self-signed certificate (legacy entry point).
///
/// Prefer [`provision`] for new code.
pub fn load_or_generate(cert_dir: &Path) -> anyhow::Result<TlsRuntime> {
    self_signed_provision(cert_dir, "127.0.0.1", &[])
}

// ── CA export / trust helpers ─────────────────────────────────────────────────

/// Return the PEM-encoded local CA certificate, or `None` if no local CA
/// has been generated yet.
pub fn export_ca_cert(cert_dir: &Path) -> anyhow::Result<Option<String>> {
    let ca_cert_path = cert_dir.join("ca-cert.pem");
    if !ca_cert_path.exists() {
        return Ok(None);
    }
    let pem = std::fs::read_to_string(&ca_cert_path)
        .with_context(|| format!("reading {}", ca_cert_path.display()))?;
    Ok(Some(pem))
}

/// Print platform-specific instructions for trusting the local CA cert.
pub fn print_ca_trust_instructions(ca_cert_path: &Path) {
    let path = ca_cert_path.display();

    info!("──────────────────────────────────────────────────────────────────────");
    info!("  New local CA generated. To make HTTPS work without browser warnings");
    info!("  trust this CA once on each device that will access the web terminal.");
    info!("");
    info!("  CA cert: {path}");
    info!("");
    info!("  Or run:  sven node install-ca   (prints platform instructions)");
    info!("──────────────────────────────────────────────────────────────────────");
}

/// Print per-platform instructions for installing the CA cert.
pub fn print_install_instructions(ca_cert_path: &Path) {
    let path = ca_cert_path.display();

    println!("CA certificate: {path}");
    println!();

    #[cfg(target_os = "macos")]
    {
        println!("── macOS ─────────────────────────────────────────────────────────────");
        println!("  sudo security add-trusted-cert -d -r trustRoot \\");
        println!("    -k /Library/Keychains/System.keychain \\");
        println!("    {path}");
        println!();
        println!("  Then restart Chrome/Safari. Firefox uses its own trust store:");
        println!("  Settings → Privacy & Security → View Certificates → Import");
    }

    #[cfg(target_os = "linux")]
    {
        println!("── Linux (system trust store, Chromium/Chrome) ───────────────────────");
        println!("  sudo cp {path} /usr/local/share/ca-certificates/sven-ca.crt");
        println!("  sudo update-ca-certificates");
        println!();
        println!("── Firefox (uses its own NSS database) ───────────────────────────────");
        println!("  # Install certutil:");
        println!("  sudo apt install libnss3-tools   # Debian/Ubuntu");
        println!("  sudo dnf install nss-tools       # Fedora/RHEL");
        println!();
        println!("  # Then for each Firefox profile:");
        println!("  for db in ~/.mozilla/firefox/*.*/; do");
        println!(r#"    certutil -A -n "Sven Local CA" -t "CT,," -i {path} -d "$db""#);
        println!("  done");
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("Import {path} as a trusted root CA in your browser's certificate manager.");
    }

    println!();
    println!("── Mobile ────────────────────────────────────────────────────────────────");
    println!("  Serve the CA cert over HTTP and open the URL on your device:");
    println!("    python3 -m http.server --directory $(dirname {path}) 8080");
    println!("    # Then open http://<your-ip>:8080/ca-cert.pem on your phone");
    println!();
    println!("  iOS:     tap the file → Settings → General → VPN & Device Management");
    println!("           → install → then Settings → General → About → Certificate");
    println!("           Trust Settings → enable full trust for the CA.");
    println!("  Android: Settings → Security → Install from storage → CA certificate");
}

// ── Certificate loading ───────────────────────────────────────────────────────

fn load_from_files(
    cert_path: &Path,
    key_path: &Path,
    mode_used: TlsModeUsed,
) -> anyhow::Result<TlsRuntime> {
    let cert_pem =
        std::fs::read(cert_path).with_context(|| format!("reading {}", cert_path.display()))?;

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
        mode_used,
    })
}

// ── SANs ──────────────────────────────────────────────────────────────────────

/// Build Subject Alternative Names for a generated server cert.
///
/// Always includes `localhost`, `127.0.0.1`, `::1`, and the machine hostname.
/// Adds the bind address if it is a specific IP (not `0.0.0.0`/`::`).
/// Appends `san_extra` from the config.
fn build_sans(bind_addr: &str, san_extra: &[String]) -> Vec<String> {
    let mut sans: Vec<String> = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];

    // Add machine hostname (e.g. "mybox" or "mybox.local").
    if let Ok(h) = hostname::get() {
        if let Some(s) = h.to_str() {
            let s = s.to_string();
            if !sans.contains(&s) {
                sans.push(s);
            }
        }
    }

    // Add the bind IP if it's a specific address (not 0.0.0.0 / ::).
    if let Some(ip_str) = bind_addr.rsplit_once(':').map(|(h, _)| h) {
        let ip_str = ip_str.trim_start_matches('[').trim_end_matches(']');
        if ip_str != "0.0.0.0" && ip_str != "::" && !sans.contains(&ip_str.to_string()) {
            sans.push(ip_str.to_string());
        }
    }

    // Append user-configured extra SANs (LAN IPs, custom hostnames, …).
    for extra in san_extra {
        let trimmed = extra.trim().to_string();
        if !trimmed.is_empty() && !sans.contains(&trimmed) {
            sans.push(trimmed);
        }
    }

    sans
}

// ── Expiry check ──────────────────────────────────────────────────────────────

fn cert_is_expiring_soon(cert_path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(cert_path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    let age_days = modified.elapsed().unwrap_or_default().as_secs() as i64 / 86400;
    age_days >= (CERT_VALIDITY_DAYS - CERT_RENEW_BEFORE_DAYS)
}

// ── File helpers ──────────────────────────────────────────────────────────────

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

fn restrict_file_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    let _ = path;
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
    use crate::config::TlsMode;

    #[test]
    fn self_signed_generates_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let rt = provision(&TlsMode::SelfSigned, dir.path(), "127.0.0.1:18790", &[]).unwrap();
        assert!(!rt.fingerprint_sha256.is_empty());
        assert!(rt.fingerprint_sha256.contains(':'));
        assert_eq!(rt.mode_used, TlsModeUsed::SelfSigned);
    }

    #[test]
    fn self_signed_cert_files_are_created() {
        let dir = tempfile::tempdir().unwrap();
        provision(&TlsMode::SelfSigned, dir.path(), "0.0.0.0:18790", &[]).unwrap();
        assert!(dir.path().join("gateway-cert.pem").exists());
        assert!(dir.path().join("gateway-key.pem").exists());
    }

    #[test]
    fn self_signed_second_load_reuses_existing() {
        let dir = tempfile::tempdir().unwrap();
        let r1 = provision(&TlsMode::SelfSigned, dir.path(), "127.0.0.1:18790", &[]).unwrap();
        let r2 = provision(&TlsMode::SelfSigned, dir.path(), "127.0.0.1:18790", &[]).unwrap();
        assert_eq!(r1.fingerprint_sha256, r2.fingerprint_sha256);
    }

    #[test]
    fn local_ca_generates_ca_and_server_cert() {
        let dir = tempfile::tempdir().unwrap();
        let rt = provision(&TlsMode::LocalCa, dir.path(), "0.0.0.0:18790", &[]).unwrap();
        assert!(dir.path().join("ca-cert.pem").exists());
        assert!(dir.path().join("ca-key.pem").exists());
        assert!(dir.path().join("gateway-cert.pem").exists());
        assert!(dir.path().join("gateway-key.pem").exists());
        assert_eq!(rt.mode_used, TlsModeUsed::LocalCa);
    }

    #[test]
    fn local_ca_reuses_ca_on_second_run() {
        let dir = tempfile::tempdir().unwrap();
        provision(&TlsMode::LocalCa, dir.path(), "127.0.0.1:18790", &[]).unwrap();
        let ca1 = std::fs::read(dir.path().join("ca-cert.pem")).unwrap();
        provision(&TlsMode::LocalCa, dir.path(), "127.0.0.1:18790", &[]).unwrap();
        let ca2 = std::fs::read(dir.path().join("ca-cert.pem")).unwrap();
        // CA cert must not change between runs.
        assert_eq!(ca1, ca2);
    }

    #[test]
    fn local_ca_san_extra_included() {
        let dir = tempfile::tempdir().unwrap();
        provision(
            &TlsMode::LocalCa,
            dir.path(),
            "0.0.0.0:18790",
            &["192.168.1.42".to_string()],
        )
        .unwrap();
        // Verify the cert was created (content verification would need x509 parser).
        assert!(dir.path().join("gateway-cert.pem").exists());
    }

    #[test]
    fn export_ca_cert_returns_none_before_first_run() {
        let dir = tempfile::tempdir().unwrap();
        assert!(export_ca_cert(dir.path()).unwrap().is_none());
    }

    #[test]
    fn export_ca_cert_returns_pem_after_local_ca() {
        let dir = tempfile::tempdir().unwrap();
        provision(&TlsMode::LocalCa, dir.path(), "127.0.0.1:18790", &[]).unwrap();
        let pem = export_ca_cert(dir.path()).unwrap().unwrap();
        assert!(pem.contains("CERTIFICATE"));
    }

    #[test]
    #[cfg(unix)]
    fn key_file_has_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        provision(&TlsMode::SelfSigned, dir.path(), "127.0.0.1:18790", &[]).unwrap();
        let meta = std::fs::metadata(dir.path().join("gateway-key.pem")).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key must be 0600, got {mode:03o}");
    }

    #[test]
    #[cfg(unix)]
    fn ca_key_has_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        provision(&TlsMode::LocalCa, dir.path(), "127.0.0.1:18790", &[]).unwrap();
        let meta = std::fs::metadata(dir.path().join("ca-key.pem")).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "CA key must be 0600, got {mode:03o}");
    }
}
