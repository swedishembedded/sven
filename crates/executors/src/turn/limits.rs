// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working inside a small
// model context window for its clients. If your team needs expertise in
// running agents on local models you can procure our services by sending an
// email to info@swedishembedded.com.
//! When a turn compacts its conversation, and how far to trust the estimate
//! of how large the request is.

use std::sync::Mutex;

/// Largest factor the server's token count may scale the estimate by; a
/// wilder ratio means the request and the estimate describe different things
/// (a cached or truncated prompt), not an estimator bias.
const MAX_ESTIMATE_SCALE: f32 = 4.0;

/// Records how the server's count of a request relates to the estimate made
/// for it. Never below 1.0 (an overcount only costs an earlier compaction when
/// the estimator is right, but a lower scale would defeat the headroom), and a
/// request that reported nothing leaves the last scale in place.
pub(super) fn record_estimate_scale(scale: &Mutex<f32>, estimated: usize, reported: u64) {
    if estimated == 0 || reported == 0 {
        return;
    }
    let ratio = (reported as f32 / estimated as f32).clamp(1.0, MAX_ESTIMATE_SCALE);
    *scale
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = ratio;
}

/// Proactive-compaction settings, mirroring the agent settings'
/// `compaction_*` fields (kept as a separate small struct so `TurnExecutor`'s
/// constructor signature doesn't grow every time this feature gains a knob).
#[derive(Clone)]
pub struct CompactionConfig {
    /// Fraction of the usable input budget at which compaction fires
    /// (0.0-1.0). See `AgentConfig::compaction_threshold`.
    pub threshold: f32,
    /// Fraction of the context window reserved for tool schemas, dynamic
    /// context, and estimation error - subtracted from `threshold` to get
    /// the effective trigger point. See `AgentConfig::compaction_overhead_reserve`.
    pub overhead_reserve: f32,
    /// Non-system messages preserved verbatim (not summarized) at the tail
    /// of the conversation. See `AgentConfig::compaction_keep_recent`.
    pub keep_recent: usize,
    /// Which prompt/format the model is asked to produce a summary in.
    /// See `AgentConfig::compaction_strategy`.
    pub strategy: sven_turn::CompactionStrategy,
}

impl Default for CompactionConfig {
    /// Matches the agent settings' default compaction fields.
    fn default() -> Self {
        Self {
            threshold: 0.85,
            overhead_reserve: 0.10,
            keep_recent: 6,
            strategy: sven_turn::CompactionStrategy::Structured,
        }
    }
}

impl CompactionConfig {
    /// The fraction of the usable input budget at which compaction should
    /// fire, after accounting for overhead reserve. Never negative.
    pub(super) fn effective_threshold(&self) -> f32 {
        (self.threshold - self.overhead_reserve).max(0.0)
    }
}
