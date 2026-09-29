// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an application can reach the built-in model drivers through the
//! facade.
//!
//! The facade's own documentation says a remote model is "a peer of the
//! local ones" hanging off the same [`ModelProvider`] seam - but the
//! built-in OpenAI/Anthropic/OpenRouter providers the docs name live in
//! `sven-model-drivers`, which the facade did not publish. An application
//! that wanted the compiled-in providers had to reach past the facade
//! (which `make check/samples` refuses for a sample) or rebuild every
//! driver on top of the trait. Re-exporting the factory here
//! is what the docs already promised: the seam is public, and so are the
//! built-ins that hang off it.

use sven_sdk::drivers::from_config;

#[test]
fn an_application_can_construct_the_mock_driver_through_the_facade() {
    // The mock provider needs no key and no network: a construction that
    // works offline proves the factory is reachable and functional.
    let config = mock_config();
    let provider = from_config(&config.model);
    assert!(
        provider.is_ok(),
        "the built-in driver factory is on the facade: {:?}",
        provider.err()
    );
}

fn mock_config() -> sven_sdk::config::Config {
    let mut config = sven_sdk::config::Config::default();
    config.model.provider = "mock".into();
    config.model.name = "mock".into();
    config
}
