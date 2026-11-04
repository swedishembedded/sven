// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! How a host configures a [`TurnExecutor`]: the optional collaborators and
//! limits it is built with.

use std::sync::Arc;

use sven_decision::telemetry::DecisionTelemetry;

use super::{CompactionConfig, ContextAssembler, TurnExecutor};

impl TurnExecutor {
    /// The directory a request's `attachments` are relative to (the
    /// session's project root); a path that leaves it is refused.
    #[must_use]
    pub fn with_attachment_root(mut self, root: Option<std::path::PathBuf>) -> Self {
        self.attachment_root = root;
        self
    }

    /// Send zero tool schemas to the model regardless of what the requesting
    /// machine asks for (`--no-tools`).
    #[must_use]
    pub fn with_no_tools(mut self, no_tools: bool) -> Self {
        self.no_tools = no_tools;
        self
    }

    /// Configure proactive compaction (see [`CompactionConfig`]).
    #[must_use]
    pub fn with_compaction_config(mut self, compaction: CompactionConfig) -> Self {
        self.compaction = compaction;
        self
    }

    /// Configure the limits on each streamed turn (see
    /// [`sven_turn::TurnLimits`]). Defaults to `TurnLimits::default()` (10% of
    /// the model's context window for thinking, 600s thinking stall, 300s
    /// stream idle) when not called.
    #[must_use]
    pub fn with_turn_limits(mut self, limits: sven_turn::TurnLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Charges what `telemetry` gathered to the next request's ledger record.
    #[must_use]
    pub fn with_decision_telemetry(mut self, telemetry: Arc<DecisionTelemetry>) -> Self {
        self.decisions = Some(telemetry);
        self
    }

    /// Builds each request's context from the history with `assembler`
    /// instead of sending the history as it is. `None` sends the history.
    #[must_use]
    pub fn with_context_assembler(mut self, assembler: Option<ContextAssembler>) -> Self {
        self.context = assembler;
        self
    }

    /// Announces the instructions whose activation holds with each request and
    /// records newly activated ones in the kernel. `None` announces nothing.
    #[must_use]
    pub fn with_instructions(
        mut self,
        instructions: Option<Arc<sven_turn::instructions::Activator>>,
    ) -> Self {
        self.instructions = instructions;
        self
    }

    /// Tells the model how much of `budget` is left, in a line at the end of
    /// every request (see [`sven_turn::TimeBudget`]). `None`, a run without a
    /// deadline, says nothing.
    #[must_use]
    pub fn with_time_budget(mut self, budget: Option<sven_turn::TimeBudget>) -> Self {
        self.time_budget = budget;
        self
    }
}
