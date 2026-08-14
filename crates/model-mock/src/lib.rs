// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `--model mock` test/dev [`sven_model::ModelProvider`] implementations:
//! [`MockProvider`] (a fixed echo responder) and [`YamlMockProvider`] (a
//! YAML-scripted responder used by end-to-end and bats tests).
//!
//! Split out of the `sven-model` god crate (refactor plan Phase 5) so that
//! crates which only need the `ModelProvider` trait + request/response types
//! do not also pull in this test scaffolding. `sven-model-drivers` depends
//! on this crate as a normal dependency (see architecture.toml's
//! `sven-model-drivers -> sven-model-mock` `[[same_layer]]` entry) because
//! `from_config`'s provider dispatch constructs a mock provider for
//! `provider: "mock"` unconditionally today; Phase 6 is expected to put that
//! arm behind a feature flag so mock support stops shipping in release
//! builds, at which point this crate becomes an optional/dev-only dependency
//! of sven-model-drivers instead of an unconditional one.

mod mock;
mod yaml_mock;

pub use mock::{MockProvider, ScriptedMockProvider};
pub use yaml_mock::YamlMockProvider;
