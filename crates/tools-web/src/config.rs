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
use crate::web_search::SearchBackend;
use crate::{WebFetchTool, WebSearchTool};

/// Which service `web_search` asks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchProvider {
    /// The sven service when one is known (signed in with `sven rc`, or a
    /// `providers.sven` entry with a key), Brave otherwise.
    #[default]
    Auto,
    /// The sven service's `/search`, billed to its account.
    Sven,
    /// Brave Search, with `api_key` or `BRAVE_API_KEY`.
    Brave,
}

/// A sven service's API: the base URL its OpenAI-compatible routes live
/// under (`https://host/v1`) and the key that authenticates them.
#[derive(Clone, PartialEq, Eq)]
pub struct SvenEndpoint {
    pub base_url: String,
    pub api_key: String,
}

impl std::fmt::Debug for SvenEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SvenEndpoint")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// The `tools.web.search` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSearchConfig {
    /// Which service searches (default `auto`).
    pub provider: SearchProvider,
    /// Brave Search API key (also checked via BRAVE_API_KEY env var)
    pub api_key: Option<String>,
    /// The sven service this process is signed in to, set at run time by
    /// `sven rc` from its sign-in; never read from or written to a file.
    #[serde(skip)]
    pub sven: Option<SvenEndpoint>,
}

impl WebSearchConfig {
    /// The backend `web_search` uses: the signed-in sven service, else
    /// `configured` (a `providers.sven` entry with a key), else Brave, as
    /// [`Self::provider`] allows.
    #[must_use]
    pub fn backend(&self, configured: Option<SvenEndpoint>) -> SearchBackend {
        let sven = self.sven.clone().or(configured);
        match (self.provider, sven) {
            (SearchProvider::Brave, _) | (SearchProvider::Auto, None) => SearchBackend::Brave {
                api_key: self.api_key.clone(),
            },
            (SearchProvider::Auto | SearchProvider::Sven, Some(endpoint)) => {
                SearchBackend::Sven(endpoint)
            }
            (SearchProvider::Sven, None) => SearchBackend::Unavailable(
                "tools.web.search.provider is sven, but no sven service is known: sign in with \
                 `sven rc`, or configure a `providers.sven` entry with its base_url and a key"
                    .into(),
            ),
        }
    }
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
    /// `web_fetch` reaches only the public internet: never this machine,
    /// its private networks or a cloud metadata service (default false; a
    /// hosted host always sets it).
    pub public_only: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            search: WebSearchConfig::default(),
            fetch_max_chars: DEFAULT_MAX_CHARS,
            public_only: false,
        }
    }
}

impl WebConfig {
    /// The keys of the `tools.web` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::fields([
            ("search", Schema::keys(&["provider", "api_key"])),
            ("fetch_max_chars", Schema::value()),
            ("public_only", Schema::value()),
        ])
    }

    /// The `web_fetch` tool, capped at [`Self::fetch_max_chars`] unless the
    /// caller asks for another length.
    #[must_use]
    pub fn fetch_tool(&self) -> WebFetchTool {
        let tool = WebFetchTool::new(self.fetch_max_chars);
        if self.public_only {
            tool.public_only()
        } else {
            tool
        }
    }

    /// The `web_search` tool, searching through [`WebSearchConfig::backend`];
    /// `configured` is the sven service a `providers.sven` entry names.
    #[must_use]
    pub fn search_tool(&self, configured: Option<SvenEndpoint>) -> WebSearchTool {
        WebSearchTool {
            backend: self.search.backend(configured),
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

    fn sven(base_url: &str) -> SvenEndpoint {
        SvenEndpoint {
            base_url: base_url.into(),
            api_key: "sk-sven".into(),
        }
    }

    #[test]
    fn without_a_sven_service_search_uses_brave_with_the_configured_key() {
        let web: WebConfig =
            serde_yaml::from_str("fetch_max_chars: 5\nsearch: {api_key: k}\n").unwrap();
        assert_eq!(web.fetch_max_chars, 5);
        assert_eq!(
            web.search_tool(None).backend,
            SearchBackend::Brave {
                api_key: Some("k".into())
            }
        );
    }

    #[test]
    fn a_known_sven_service_is_the_default_search_backend() {
        let mut web: WebConfig = serde_yaml::from_str("search: {api_key: k}\n").unwrap();
        // A `providers.sven` entry with a key.
        assert_eq!(
            web.search_tool(Some(sven("https://configured/v1"))).backend,
            SearchBackend::Sven(sven("https://configured/v1"))
        );
        // Signed in with `sven rc`: that service, before a configured one.
        web.search.sven = Some(sven("https://signed-in/v1"));
        assert_eq!(
            web.search_tool(Some(sven("https://configured/v1"))).backend,
            SearchBackend::Sven(sven("https://signed-in/v1"))
        );
    }

    #[test]
    fn provider_brave_keeps_brave_and_provider_sven_needs_a_service() {
        let mut web: WebConfig =
            serde_yaml::from_str("search: {provider: brave, api_key: k}\n").unwrap();
        web.search.sven = Some(sven("https://signed-in/v1"));
        assert_eq!(
            web.search_tool(None).backend,
            SearchBackend::Brave {
                api_key: Some("k".into())
            }
        );
        let web: WebConfig = serde_yaml::from_str("search: {provider: sven}\n").unwrap();
        assert!(
            matches!(web.search_tool(None).backend, SearchBackend::Unavailable(reason) if reason.contains("sven rc"))
        );
    }

    #[test]
    fn the_signed_in_key_is_never_written_out_or_logged() {
        let mut web = WebConfig::default();
        web.search.sven = Some(SvenEndpoint {
            base_url: "https://s/v1".into(),
            api_key: "sk-secret-token".into(),
        });
        assert!(!serde_yaml::to_string(&web)
            .unwrap()
            .contains("sk-secret-token"));
        assert!(!format!("{web:?}").contains("sk-secret-token"));
    }
}
