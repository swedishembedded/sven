// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `tools.web` and `tools.lints` sections of the configuration file.
//!
//! Swedish Embedded AB implements web research and code-diagnostic tooling
//! for agents for its clients. If your team needs expertise in giving
//! language-model agents safe access to the web and to a project's linters
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use serde::{Deserialize, Serialize};
use sven_config::Schema;

use crate::web_fetch::DEFAULT_MAX_CHARS;
use crate::{WebFetchTool, WebSearchTool};

/// The `tools.web.search` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSearchConfig {
    /// Brave Search API key (also checked via BRAVE_API_KEY env var)
    pub api_key: Option<String>,
}

/// The `tools.web` section: what `web_fetch` and `web_search` are given.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Search backend configuration
    #[serde(default)]
    pub search: WebSearchConfig,
    /// Default maximum characters for web_fetch (default 20000)
    pub fetch_max_chars: usize,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            search: WebSearchConfig::default(),
            fetch_max_chars: DEFAULT_MAX_CHARS,
        }
    }
}

impl WebConfig {
    /// The keys of the `tools.web` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::fields([
            ("search", Schema::keys(&["api_key"])),
            ("fetch_max_chars", Schema::value()),
        ])
    }

    /// The `web_fetch` tool, capped at [`Self::fetch_max_chars`] unless the
    /// caller asks for another length.
    #[must_use]
    pub fn fetch_tool(&self) -> WebFetchTool {
        WebFetchTool::new(self.fetch_max_chars)
    }

    /// The `web_search` tool, with the configured API key.
    #[must_use]
    pub fn search_tool(&self) -> WebSearchTool {
        WebSearchTool {
            api_key: self.search.api_key.clone(),
        }
    }
}

/// The `tools.lints` section: the command that lints each kind of project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LintsConfig {
    /// Override the lint command for Rust projects
    pub rust_command: Option<String>,
    /// Override the lint command for TypeScript/JS projects
    pub typescript_command: Option<String>,
    /// Override the lint command for Python projects
    pub python_command: Option<String>,
}

impl LintsConfig {
    /// The keys of the `tools.lints` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&["rust_command", "typescript_command", "python_command"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fetch_cap_defaults_to_twenty_thousand_characters() {
        assert_eq!(WebConfig::default().fetch_max_chars, 20_000);
    }

    #[test]
    fn the_configured_key_reaches_the_search_tool() {
        let web: WebConfig =
            serde_yaml::from_str("fetch_max_chars: 5\nsearch: {api_key: k}\n").unwrap();
        assert_eq!(web.fetch_max_chars, 5);
        assert_eq!(web.search_tool().api_key.as_deref(), Some("k"));
    }
}
