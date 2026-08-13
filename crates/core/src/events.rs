// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

/// Events emitted by the agent during a single turn.
/// Consumers (CI runner, TUI) subscribe to these to drive their output.
///
/// Re-exports [`sven_vocab::SessionEvent`] — the single, unified session
/// event stream (`sven-hsm` re-exports the same type as `UiEvent`).
pub use sven_vocab::SessionEvent as AgentEvent;
pub use sven_vocab::{CompactionStrategyUsed, PeerInfo};
