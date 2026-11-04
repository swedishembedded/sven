// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The crate's integration tests, linked as one binary rather than one per
//! file: each links the whole dependency tree, so a binary per file
//! multiplied build time and disk for nothing. A test here must not change
//! process-wide state (the current directory, environment variables): the
//! others run beside it.

mod package;
mod plan;
mod publish;
mod workflow;
mod workflow_attach;
mod workflow_gaps;
mod workflow_gates;
mod workflow_repair_allowance;
mod workflow_roles;
mod workflow_schema;
mod workflow_scratchpad;
mod workflow_script_gates;
