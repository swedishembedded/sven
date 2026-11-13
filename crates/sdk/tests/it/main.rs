// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The crate's integration tests, linked as one binary rather than one per
//! file: each links the whole dependency tree, so a binary per file
//! multiplied build time and disk for nothing. A test here must not change
//! process-wide state (the current directory, environment variables): the
//! others run beside it. The three that do (`deferred_tools`,
//! `project_root`, `tool_footprint`) keep binaries of their own.

#[path = "../common/mod.rs"]
mod common;
#[path = "../host_common/mod.rs"]
mod host_common;

mod agent_lifecycle;
mod agent_macro;
mod approvals;
mod builtin_drivers;
mod call_records;
mod call_timing;
mod chain;
mod compaction_recall;
mod configuration;
mod definitions;
mod event_stream;
mod extension;
mod fork;
mod gates;
mod goal;
mod harness_authz;
mod harness_child_records;
mod harness_children;
mod harness_compile;
mod harness_conversation_trace;
mod harness_cut_off;
mod harness_decoding;
mod harness_empty_reply;
mod harness_gate_inputs;
mod harness_host_policy;
mod harness_imports;
mod harness_item_checkpoints;
mod harness_item_conversations;
mod harness_publish;
mod harness_reasoning;
mod harness_replay;
mod harness_roles;
mod harness_run;
mod harness_scratchpad;
mod harness_stress;
mod harness_system_variants;
mod harness_trajectory_metrics;
mod harness_truncated;
mod harness_watchdog;
mod harness_workspace;
mod history;
mod journal_kill;
#[cfg(feature = "sqlite-memory")]
mod memory_recall;
#[cfg(feature = "sqlite-memory")]
mod memory_scope;
mod modes;
mod parked_approvals;
mod project_root_children;
mod questions;
mod read_conversations;
mod refused_calls;
mod reproduce;
mod round_budget;
mod run_bounds;
mod run_decoding;
mod search;
mod seat;
mod state_version;
mod tool_rounds;
mod toolsets;
mod trajectory;
mod trajectory_fidelity;
mod trajectory_metrics;
mod transient_retries;
mod typed_methods;
