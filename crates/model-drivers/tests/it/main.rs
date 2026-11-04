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

mod all_drivers_mock;
mod brain_thinking_wire;
mod dbus_provider_test;
mod discovery_tests;
mod driver_tests;
mod embedding_wire;
mod manifest_wire;
mod openai_stream_contract;
mod provider_check;
mod response_format_wire;
mod sampling_wire;
mod tool_choice_wire;
mod transcription_wire;
mod wire_format_tests;
