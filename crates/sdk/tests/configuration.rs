// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an application can reach a model without naming a kernel crate.
//!
//! `EngineBuilder::config` takes a `Config`, so an application that wants any
//! model other than the compiled-in default has to be able to name and load
//! that type. Without it on the facade, the only way to configure an engine is
//! to depend on `sven-config` directly - which makes the facade insufficient
//! for the first thing every real application does.

use sven_sdk::config::{load, Config};
use sven_sdk::Engine;

#[test]
fn an_application_can_name_and_default_the_configuration() {
    let config = Config::default();
    Engine::builder()
        .config(config)
        .build()
        .expect("an engine builds on a configuration the application supplied");
}

#[test]
fn an_application_can_load_svens_own_configuration() {
    // The loader reads files and the environment; whether this machine has
    // either is not the point. The spec is that an application can *call* it
    // through the facade and hand the result to the builder.
    let loaded = load(None).expect("configuration loads");
    Engine::builder()
        .config(loaded)
        .build()
        .expect("an engine builds on the loaded configuration");
}
