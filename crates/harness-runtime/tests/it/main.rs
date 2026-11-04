// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The crate's integration tests, linked as one binary rather than one per
//! file: each links the whole dependency tree, so a binary per file
//! multiplied build time and disk for nothing. A test here must not change
//! process-wide state (the current directory, environment variables): the
//! others run beside it.

#[path = "../common/mod.rs"]
mod common;

mod harness_agent;
mod harness_answer_repair;
mod harness_approval;
mod harness_checkpoint;
mod harness_checkpoint_interruptions;
mod harness_conversations;
mod harness_corrections;
mod harness_decoding;
mod harness_gaps;
mod harness_gate;
mod harness_gate_data;
mod harness_item_failure;
mod harness_iterate;
mod harness_machine;
mod harness_registry;
mod harness_scratchpad;
mod harness_script_gate;
mod harness_steps;
mod harness_stop;
mod harness_stop_gate;
mod harness_trace;
mod harness_vars;
