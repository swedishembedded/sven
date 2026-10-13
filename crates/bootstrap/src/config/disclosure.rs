// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in deferred tool
// disclosure for agents then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The `tools.disclosure` section: how much of each tool's definition a
//! model request carries.

use serde::{Deserialize, Serialize};
use sven_config::Schema;
use sven_tool_registry::Disclosure;

/// `tools.disclosure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisclosureConfig {
    /// `full` sends every tool's definition with every request. `deferred`
    /// sends the core tools' and the loaded tools' and describes the rest in
    /// one line each, to be loaded through the `tools` tool.
    ///
    /// The default of a configuration built in code is `full`; one read from
    /// a file that does not set it is `deferred` (see `Config::from_document`).
    pub mode: Disclosure,
}

impl Default for DisclosureConfig {
    fn default() -> Self {
        Self {
            mode: Disclosure::Full,
        }
    }
}

impl DisclosureConfig {
    /// The keys of the `tools.disclosure` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&["mode"])
    }
}
