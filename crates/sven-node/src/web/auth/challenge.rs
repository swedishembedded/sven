// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! In-memory challenge store for WebAuthn registration and authentication.
//!
//! WebAuthn ceremonies are two-round-trips:
//! 1. Server generates a challenge → sent to browser.
//! 2. Browser performs the ceremony → sends result back.
//!
//! The server must hold the ceremony state between these two requests.
//! This module provides a thread-safe TTL store that pairs a challenge ID
//! (UUID sent to the browser as a cookie) with the opaque ceremony state.
//!
//! # Expiry
//!
//! Entries expire after [`CHALLENGE_TTL_SECS`] seconds.  A background task
//! sweeps the map every [`SWEEP_INTERVAL_SECS`] seconds.  This prevents
//! memory leaks from incomplete ceremonies.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use dashmap::DashMap;
use tokio::task::JoinHandle;
use uuid::Uuid;
use webauthn_rs::prelude::{PasskeyAuthentication, PasskeyRegistration};

/// Ceremony states live for 5 minutes.
const CHALLENGE_TTL_SECS: u64 = 300;

/// Sweep expired entries every 60 seconds.
const SWEEP_INTERVAL_SECS: u64 = 60;

// ── Challenge state variants ──────────────────────────────────────────────────

/// Pending WebAuthn ceremony state — either registration or authentication.
pub enum ChallengeState {
    /// A passkey registration ceremony is in progress for a new device.
    Registration {
        state: PasskeyRegistration,
        /// Pre-assigned device UUID (emitted in the JSON response so the
        /// browser can identify itself while waiting for approval).
        device_id: Uuid,
    },
    /// A passkey authentication ceremony is in progress for an existing device.
    Authentication {
        state: PasskeyAuthentication,
        /// The device that initiated authentication.
        device_id: Uuid,
    },
}

struct Entry {
    state: ChallengeState,
    created_at: Instant,
}

// ── Store ─────────────────────────────────────────────────────────────────────

/// Thread-safe, TTL-bounded challenge store.
///
/// Clone is cheap (`Arc` inside).
#[derive(Clone)]
pub struct ChallengeStore {
    map: Arc<DashMap<Uuid, Entry>>,
}

impl ChallengeStore {
    /// Create a new store and spawn the background sweep task.
    ///
    /// The returned `JoinHandle` can be ignored — the task terminates when the
    /// store is dropped (all `Arc` references released).
    pub fn new() -> (Self, JoinHandle<()>) {
        let map = Arc::new(DashMap::<Uuid, Entry>::new());
        let sweep_map = Arc::clone(&map);
        let handle = tokio::spawn(async move {
            let ttl = Duration::from_secs(CHALLENGE_TTL_SECS);
            let interval = Duration::from_secs(SWEEP_INTERVAL_SECS);
            loop {
                tokio::time::sleep(interval).await;
                let now = Instant::now();
                sweep_map.retain(|_, entry| now.duration_since(entry.created_at) < ttl);
            }
        });
        (Self { map }, handle)
    }

    /// Insert a challenge state, returning the challenge ID to send to the browser.
    pub fn insert(&self, state: ChallengeState) -> Uuid {
        let challenge_id = Uuid::new_v4();
        self.map.insert(
            challenge_id,
            Entry {
                state,
                created_at: Instant::now(),
            },
        );
        challenge_id
    }

    /// Remove and return the challenge state for a given challenge ID.
    ///
    /// Returns `None` if the challenge ID is unknown or expired.
    pub fn take(&self, challenge_id: Uuid) -> Option<ChallengeState> {
        let entry = self.map.remove(&challenge_id)?;
        let ttl = Duration::from_secs(CHALLENGE_TTL_SECS);
        if Instant::now().duration_since(entry.1.created_at) > ttl {
            return None;
        }
        Some(entry.1.state)
    }
}

impl Default for ChallengeStore {
    fn default() -> Self {
        let (store, _) = Self::new();
        store
    }
}
