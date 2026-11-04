// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The crate's integration tests, linked as one binary rather than one per
//! file: each links the whole dependency tree, so a binary per file
//! multiplied build time and disk for nothing. A test here must not change
//! process-wide state (the current directory, environment variables): the
//! others run beside it; `tool_disclosure`, `tool_summaries` keep binaries of their own.

mod approval_modes;
mod cache_aware;
mod context_reconstruct;
mod decision_ranker;
mod decision_ranker_skills;
mod live_ui_test_dispatch;
mod memory_home;
mod memory_import;
