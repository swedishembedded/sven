// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The crate's integration tests, linked as one binary rather than one per
//! file: each links the whole dependency tree, so a binary per file
//! multiplied build time and disk for nothing. A test here must not change
//! process-wide state (the current directory, environment variables): the
//! others run beside it.

mod cache_gate;
mod compact_on_demand;
mod context_assembly;
mod decision_trace;
mod deferred_calls;
mod empty_turn;
mod estimate_calibration;
mod fork_compaction;
mod instructions;
mod kernel_policy_tests;
mod overflow_retry;
mod pinned_state;
mod pipeline_integration;
mod served_model_notices;
mod time_budget;
mod turn_end;
mod turn_output_cap;
mod verify_command;
mod watchdog_abort;
