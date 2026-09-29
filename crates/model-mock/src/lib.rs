// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `--model mock` test/dev [`sven_model::ModelProvider`] implementations:
//! [`MockProvider`] (a fixed echo responder) and [`YamlMockProvider`] (a
//! YAML-scripted responder used by end-to-end and bats tests).
//!
//! A crate of its own so that crates which only need the `ModelProvider`
//! trait + request/response types do not also pull in this test
//! scaffolding. `sven-model-drivers` depends on this crate as a normal
//! dependency (see architecture.toml's `sven-model-drivers ->
//! sven-model-mock` `[[same_layer]]` entry) because `from_config`'s provider
//! dispatch constructs a mock provider for `provider: "mock"`
//! unconditionally, so mock support ships in release builds. Putting that
//! arm behind a feature flag would make this an optional/dev-only
//! dependency of sven-model-drivers.

mod mock;
mod yaml_mock;

pub use mock::{FailingMockProvider, MockProvider, ScriptedMockProvider};
pub use yaml_mock::YamlMockProvider;
