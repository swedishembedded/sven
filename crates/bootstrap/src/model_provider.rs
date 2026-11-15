// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The model provider a session builds from its configuration.

use std::sync::Arc;

use sven_model::ModelProvider;
use sven_model_drivers::ModelConfig;

/// The provider `config` describes.
///
/// `from_config_probed` additionally asks the live server for its actual
/// context window (a short-timeout, best-effort HTTP call) and clamps a
/// hand-written `max_tokens` down to what the server can really serve - this
/// is what stops sven from building a prompt the server will reject after
/// already admitting the request. Building a session already does I/O (the MCP
/// connect), so this adds no new purity concern; it is a builder, not a
/// transition.
pub(crate) async fn probed(config: &ModelConfig) -> anyhow::Result<Arc<dyn ModelProvider>> {
    Ok(Arc::from(
        sven_model_drivers::from_config_probed(config).await?,
    ))
}
