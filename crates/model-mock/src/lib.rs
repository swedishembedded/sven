// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `--model mock` test/dev [`sven_model::ModelProvider`] implementations:
//! [`MockProvider`] (a fixed echo responder) and [`YamlMockProvider`] (a
//! YAML-scripted responder used by end-to-end and bats tests), plus
//! [`StallingMockProvider`], which goes quiet on purpose to exercise the
//! turn watchdogs, and [`CallbackProvider`], whose reply is a function of the
//! request.
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

mod callback;
mod mock;
mod recovery;
// Goes quiet on tokio's clock, which tests pause; native only.
#[cfg(not(target_arch = "wasm32"))]
mod stalling;
mod yaml_mock;

pub use callback::CallbackProvider;
pub use mock::{FailingMockProvider, MockProvider, ScriptedMockProvider};
pub use recovery::{RecoverCall, RecoverRule};
#[cfg(not(target_arch = "wasm32"))]
pub use stalling::{StallMode, StallingMockProvider};
pub use yaml_mock::YamlMockProvider;
