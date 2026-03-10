// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Device registry for web terminal passkey authentication.
//!
//! Each browser that registers a WebAuthn passkey becomes a `DeviceRecord`.
//! New registrations start in `Pending` state; an admin must run
//! `sven node web-devices approve <id>` to grant access.
//!
//! # Persistence
//!
//! The registry is stored as YAML at `~/.config/sven/node/web_devices.yaml`
//! (configurable).  Writes are atomic: data is written to a temp file in the
//! same directory, then renamed into place.  File permissions are `0o600`.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;
use webauthn_rs::prelude::Passkey;

// ── Domain types ──────────────────────────────────────────────────────────────

/// Lifecycle status of a registered browser device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceStatus {
    /// Registered but not yet approved by an admin.
    Pending,
    /// Approved — the device may open PTY sessions.
    Approved,
    /// Revoked — the device is blocked; any open sessions are killed.
    Revoked,
}

impl std::fmt::Display for DeviceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Approved => write!(f, "approved"),
            Self::Revoked => write!(f, "revoked"),
        }
    }
}

/// A registered browser/device with its WebAuthn credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRecord {
    /// Stable device UUID. Shown to users as the "device ID".
    pub id: Uuid,

    /// Human-readable label (e.g. "Martin's iPhone" or "browser-abc1").
    pub display_name: String,

    /// WebAuthn credential created during registration.
    ///
    /// Serializable behind the `danger-allow-state-serialisation` feature.
    pub credential: Passkey,

    /// Current status.
    pub status: DeviceStatus,

    /// When the device was first registered.
    pub created_at: DateTime<Utc>,

    /// When status was set to `Approved` (None if still pending/revoked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<DateTime<Utc>>,

    /// Last successful authentication timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<DateTime<Utc>>,
}

impl DeviceRecord {
    /// Short ID (first 8 hex chars of the UUID) for PTY session names.
    pub fn short_id(&self) -> String {
        self.id.to_string().replace('-', "")[..8].to_string()
    }
}

// ── On-disk format ────────────────────────────────────────────────────────────

/// YAML envelope written to the devices file.
///
/// Stores the `rp_id` that was active when the credentials were registered so
/// that a config change can be detected at startup and stale credentials can
/// be purged automatically instead of producing opaque `NotAllowedError`s in
/// the browser.
///
/// Old files (flat `Vec<DeviceRecord>`) are detected and migrated on first
/// load.
#[derive(Debug, Serialize, Deserialize)]
struct RegistryFile {
    /// WebAuthn RP ID in effect when these credentials were registered.
    rp_id: String,
    /// The device records.
    devices: Vec<DeviceRecord>,
}

// ── Registry ──────────────────────────────────────────────────────────────────

/// Thread-safe, persistently-backed device registry.
///
/// Clone is cheap — the inner state is `Arc<RwLock<...>>`.
#[derive(Clone)]
pub struct DeviceRegistry {
    inner: Arc<RwLock<RegistryInner>>,
}

struct RegistryInner {
    devices: Vec<DeviceRecord>,
    path: PathBuf,
    rp_id: String,
}

impl DeviceRegistry {
    /// Load from disk and verify that the stored RP ID matches `current_rp_id`.
    ///
    /// If the file is absent an empty registry is created.  If the RP ID has
    /// changed since the last run, all existing credentials are purged and a
    /// clear warning is logged — this prevents the silent `NotAllowedError`
    /// that browsers return when a passkey is presented for the wrong RP ID.
    ///
    /// Old files written as a plain `Vec<DeviceRecord>` (without the RP ID
    /// envelope) are migrated transparently: they are treated as if the RP ID
    /// was `localhost` (the historical default).
    pub fn load(path: &Path, current_rp_id: &str) -> anyhow::Result<Self> {
        let (mut devices, stored_rp_id) = if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;

            // Try new envelope format first, fall back to legacy flat list.
            if let Ok(file) = serde_yaml::from_str::<RegistryFile>(&text) {
                (file.devices, file.rp_id)
            } else {
                let list = serde_yaml::from_str::<Vec<DeviceRecord>>(&text)
                    .with_context(|| format!("parsing {}", path.display()))?;
                // Legacy files had no RP ID stored — treat as localhost.
                (list, "localhost".to_string())
            }
        } else {
            (Vec::new(), current_rp_id.to_string())
        };

        // Detect RP ID change and purge stale credentials.
        if stored_rp_id != current_rp_id {
            let count = devices.len();
            devices.clear();
            warn!(
                old_rp_id = %stored_rp_id,
                new_rp_id = %current_rp_id,
                purged    = count,
                "web.rp_id changed — purging {} stale credential(s). \
                 All browsers must re-register at https://{}",
                count, current_rp_id,
            );
        }

        info!(
            path  = %path.display(),
            rp_id = %current_rp_id,
            count = devices.len(),
            "web device registry loaded"
        );

        let inner = RegistryInner {
            devices,
            path: path.to_path_buf(),
            rp_id: current_rp_id.to_string(),
        };
        // Persist immediately so the new rp_id (and any purge) is written.
        inner.persist()?;

        Ok(Self {
            inner: Arc::new(RwLock::new(inner)),
        })
    }

    /// Register a new device in `Pending` state.
    pub async fn register(
        &self,
        id: Uuid,
        display_name: String,
        credential: Passkey,
    ) -> anyhow::Result<DeviceRecord> {
        let record = DeviceRecord {
            id,
            display_name,
            credential,
            status: DeviceStatus::Pending,
            created_at: Utc::now(),
            approved_at: None,
            last_seen: None,
        };
        let mut inner = self.inner.write().await;
        inner.devices.push(record.clone());
        inner.persist()?;
        info!(device_id = %id, "web device registered (pending approval)");
        Ok(record)
    }

    /// Retrieve a device by ID (read-only snapshot).
    pub async fn get(&self, id: Uuid) -> Option<DeviceRecord> {
        let inner = self.inner.read().await;
        inner.devices.iter().find(|d| d.id == id).cloned()
    }

    /// Approve a pending device. Returns `false` if the device was not found.
    pub async fn approve(&self, id: Uuid) -> anyhow::Result<bool> {
        let mut inner = self.inner.write().await;
        let Some(device) = inner.devices.iter_mut().find(|d| d.id == id) else {
            return Ok(false);
        };
        if device.status == DeviceStatus::Revoked {
            warn!(device_id = %id, "cannot approve a revoked device");
            return Ok(false);
        }
        device.status = DeviceStatus::Approved;
        device.approved_at = Some(Utc::now());
        inner.persist()?;
        info!(device_id = %id, "web device approved");
        Ok(true)
    }

    /// Revoke a device. Returns `false` if the device was not found.
    pub async fn revoke(&self, id: Uuid) -> anyhow::Result<bool> {
        let mut inner = self.inner.write().await;
        let Some(device) = inner.devices.iter_mut().find(|d| d.id == id) else {
            return Ok(false);
        };
        device.status = DeviceStatus::Revoked;
        inner.persist()?;
        info!(device_id = %id, "web device revoked");
        Ok(true)
    }

    /// Update `last_seen` timestamp for a device (called on successful login).
    pub async fn touch(&self, id: Uuid) -> anyhow::Result<()> {
        let mut inner = self.inner.write().await;
        if let Some(device) = inner.devices.iter_mut().find(|d| d.id == id) {
            device.last_seen = Some(Utc::now());
            inner.persist()?;
        }
        Ok(())
    }

    /// Update the stored credential counter after a successful authentication.
    ///
    /// webauthn-rs requires this to prevent credential cloning attacks.
    pub async fn update_credential(&self, id: Uuid, credential: Passkey) -> anyhow::Result<()> {
        let mut inner = self.inner.write().await;
        if let Some(device) = inner.devices.iter_mut().find(|d| d.id == id) {
            device.credential = credential;
            inner.persist()?;
        }
        Ok(())
    }

    /// Return all devices, optionally filtered by status.
    pub async fn list(&self, filter: Option<DeviceStatus>) -> Vec<DeviceRecord> {
        let inner = self.inner.read().await;
        inner
            .devices
            .iter()
            .filter(|d| filter.as_ref().is_none_or(|f| &d.status == f))
            .cloned()
            .collect()
    }
}

impl RegistryInner {
    /// Atomically persist devices to disk (write temp → rename).
    fn persist(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating dir {}", parent.display()))?;
        }

        let file = RegistryFile {
            rp_id: self.rp_id.clone(),
            devices: self.devices.clone(),
        };
        let yaml = serde_yaml::to_string(&file).context("serializing device registry")?;

        let tmp = self.path.with_extension("yaml.tmp");
        std::fs::write(&tmp, &yaml)
            .with_context(|| format!("writing temp file {}", tmp.display()))?;

        // Set restrictive permissions before rename.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .context("setting file permissions on device registry")?;
        }

        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming {} → {}", tmp.display(), self.path.display()))?;
        Ok(())
    }
}

// ── Default path ──────────────────────────────────────────────────────────────

pub fn default_devices_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/sven/node/web_devices.yaml")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Registering a device and checking status.
    // We can't easily unit-test WebAuthn credentials (they require a ceremony),
    // so these tests focus on the registry mechanics, not the credential type.
    // Integration tests will cover the full WebAuthn flow.

    #[test]
    fn device_status_display() {
        assert_eq!(DeviceStatus::Pending.to_string(), "pending");
        assert_eq!(DeviceStatus::Approved.to_string(), "approved");
        assert_eq!(DeviceStatus::Revoked.to_string(), "revoked");
    }

    #[test]
    fn default_devices_path_is_under_home() {
        let p = default_devices_path();
        assert!(p.to_string_lossy().contains("web_devices.yaml"));
    }
}
