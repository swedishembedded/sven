// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! `provider: dbus` - brain's D-Bus transport, when this build has it.

use sven_config::ModelConfig;
use sven_model::ModelProvider;

/// The D-Bus provider for `cfg`. It connects lazily, on the first request.
#[cfg(all(unix, feature = "dbus"))]
pub(crate) fn provider(
    cfg: &ModelConfig,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    use sven_model::dbus::{DbusOptions, DbusProvider};
    Ok(Box::new(DbusProvider::new(
        cfg.name.clone(),
        DbusOptions::from_driver_options(&cfg.driver_options),
        max_tokens,
        temperature,
    )))
}

/// Without the transport the provider name is still recognised, and refused
/// with the reason instead of falling through to the HTTP catch-all.
#[cfg(not(all(unix, feature = "dbus")))]
pub(crate) fn provider(
    _cfg: &ModelConfig,
    _max_tokens: Option<u32>,
    _temperature: Option<f32>,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    anyhow::bail!(
        "the 'dbus' model provider is not available in this build \
         (requires a Unix target and the `dbus` feature)"
    )
}
