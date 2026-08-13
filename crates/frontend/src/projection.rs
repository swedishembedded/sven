// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Re-exports [`sven_session_model`]'s `MachineProjection` and adds the
//! tokio broadcast plumbing a frontend uses to carry it from the kernel task
//! to the UI. `MachineProjection` itself moved to `sven-session-model`
//! (pure, no tokio) so TUI and CI can share the same derivation logic; this
//! module keeps only what genuinely needs an async runtime.

pub use sven_session_model::{projection_to_session_state, MachineProjection};

// ── ProjectionBroadcast ───────────────────────────────────────────────────────

/// A broadcast channel carrying [`MachineProjection`] updates.
///
/// The kernel task (or bridge task) publishes after every dispatch; the TUI,
/// GUI, and node control service subscribe for rendering.
pub type ProjectionTx = tokio::sync::broadcast::Sender<MachineProjection>;
pub type ProjectionRx = tokio::sync::broadcast::Receiver<MachineProjection>;

/// Creates a `(tx, rx)` pair for broadcasting projections.
pub fn projection_channel(capacity: usize) -> (ProjectionTx, ProjectionRx) {
    tokio::sync::broadcast::channel(capacity)
}
