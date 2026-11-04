// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The crate's integration tests, linked as one binary rather than one per
//! file: each links the whole dependency tree, so a binary per file
//! multiplied build time and disk for nothing. A test here must not change
//! process-wide state (the current directory, environment variables): the
//! others run beside it.

mod assembly;
mod image_limits;
mod instruction_index;
mod instructions;
mod ledger_recovery;
mod model_stall;
mod openai_compat_stall;
mod prompt_sections;
mod reconstruct;
mod request_manifest;
mod system_prompt_golden;
mod tool_call_identity;
mod turn_decoding;
