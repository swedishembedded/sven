// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The configuration a sven session runs under: the model and the endpoints
//! it can be reached through, the agent loop's bounds, the tools and the MCP
//! servers.
//!
//! `sven-config` finds, merges and expands the configuration file but knows
//! none of its sections. Each section's types, defaults and validation belong
//! to the crate that reads it - [`ModelConfig`] and
//! [`sven_model_drivers::ProviderEntry`] to `sven-model-drivers`,
//! [`sven_mcp_client::McpServerConfig`] to `sven-mcp-client`, each tool's
//! settings to its own crate - and this module is where they are put together
//! into the [`Config`] a session is built from, and where the sections owned
//! by the runtime itself ([`AgentConfig`], [`ToolsConfig`]) are defined.
//!
//! Swedish Embedded AB implements configuration for agent runtimes for its
//! clients. If your team needs expertise in layered, validated configuration
//! for language-model agents then you can procure our services by sending an
//! email to info@swedishembedded.com.

mod agent;
mod tools;

use std::path::Path;

use serde::{Deserialize, Serialize};
use sven_config::{ConfigDocument, Schema};
use sven_mcp_client::{mcp_servers_schema, McpServers};
use sven_model_drivers::{providers_schema, ModelConfig, Providers};

pub use agent::AgentConfig;
pub use tools::ToolsConfig;

/// Everything a session is configured by.
///
/// Every section is optional in the file; an absent one has its defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The active model.
    pub model: ModelConfig,
    /// The agent loop's bounds and compaction.
    pub agent: AgentConfig,
    /// The tools and what they are allowed.
    pub tools: ToolsConfig,
    /// Named endpoints a model can be reached through. Reference one by name
    /// in `model.provider`, and one of its models in `model.name`.
    ///
    /// Any string value in the file may carry `${VAR}` or `${VAR:-default}`,
    /// expanded when the file is loaded - the way to keep an API key out of a
    /// file that is under version control.
    pub providers: Providers,
    /// External MCP (Model Context Protocol) servers.
    ///
    /// Each entry is keyed by a short identifier used as the tool prefix. For
    /// example, a server named `"github"` exposes tools as
    /// `"github-list_repos"`.
    ///
    /// When sven adds an MCP server via the `system` tool, it writes to the
    /// nearest `.sven/config.yaml` (the last override layer).
    pub mcp_servers: McpServers,
}

impl Config {
    /// The keys of the sections this type reads.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::fields([
            ("model", ModelConfig::schema()),
            ("agent", AgentConfig::schema()),
            ("tools", ToolsConfig::schema()),
            ("providers", providers_schema()),
            ("mcp_servers", mcp_servers_schema()),
        ])
    }

    /// Loads the configuration the way every sven program does: every file
    /// found (see [`ConfigDocument::load`]), then `extra` - the `--config`
    /// flag - over them.
    ///
    /// A key no section recognises is logged as a warning, and so is a file
    /// that cannot be read as a configuration (it is then ignored, with the
    /// defaults in force). The `tui` section is the interactive frontend's;
    /// it is in the same file, and is not reported here.
    ///
    /// # Errors
    ///
    /// A file that exists but cannot be read or is not YAML, and an `extra`
    /// path that cannot be read.
    pub fn load(extra: Option<&Path>) -> anyhow::Result<Self> {
        let document = ConfigDocument::load(extra)?;
        document.warn_unknown_keys(&Self::schema().with("tui", Schema::value()));
        Ok(Self::from_document(&document))
    }

    /// The configuration `document` holds, settled ([`Self::settle`]).
    #[must_use]
    pub fn from_document(document: &ConfigDocument) -> Self {
        let mut config: Self = document.decode();
        config.settle(document.contains("model"));
        config
    }

    /// Settles the active model once the file is read: chosen from the
    /// environment when the file names none (`model_configured` false),
    /// expanded from the named provider it refers to, and checked against
    /// the token limits it sets. See [`sven_model_drivers::config::settle`].
    pub fn settle(&mut self, model_configured: bool) {
        sven_model_drivers::config::settle(&mut self.model, &self.providers, model_configured);
    }

    /// The model `spec` names - `provider/model`, a named provider, a catalog
    /// model or a bare model name - resolved against the active model and the
    /// named providers (see [`sven_model_drivers::ModelResolver`]).
    #[must_use]
    pub fn resolve_model(&self, spec: &str) -> ModelConfig {
        sven_model_drivers::resolve_model_from_config(&self.model, &self.providers, spec)
    }

    /// `provider/model` naming the active model so that another sven process
    /// loading the same configuration resolves the same endpoint, key and
    /// limits. See [`sven_model_drivers::config::model_reference`].
    #[must_use]
    pub fn model_reference(&self) -> String {
        sven_model_drivers::config::model_reference(&self.model, &self.providers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(yaml: &str) -> ConfigDocument {
        ConfigDocument::from_yaml(yaml).unwrap()
    }

    #[test]
    fn every_section_is_optional() {
        let config = Config::from_document(&document("agent: {max_tool_rounds: 7}\n"));
        assert_eq!(config.agent.max_tool_rounds, 7);
        assert_eq!(
            config.tools.timeout_secs,
            ToolsConfig::default().timeout_secs
        );
        assert!(config.providers.is_empty() && config.mcp_servers.is_empty());
    }

    #[test]
    fn a_section_the_file_leaves_partial_does_not_discard_the_rest() {
        let config = Config::from_document(&document(
            "model: {provider: anthropic, name: claude-opus-4-6}\n\
             agent: {compaction_keep_recent: 3}\n\
             tools: {timeout_secs: 99}\n",
        ));
        assert_eq!(config.model.provider, "anthropic");
        assert_eq!(config.agent.compaction_keep_recent, 3);
        assert_eq!(config.agent.max_tool_rounds, 200);
        assert_eq!(config.tools.timeout_secs, 99);
    }

    #[test]
    fn a_named_provider_is_expanded_into_the_active_model() {
        let config = Config::from_document(&document(
            "model: {provider: my_ollama, name: my-model}\n\
             providers:\n  my_ollama:\n    name: openai\n    base_url: http://localhost:8000/v1\n    \
             models:\n      my-model:\n        max_tokens: 54272\n",
        ));
        assert_eq!(config.model.provider, "openai");
        assert_eq!(config.model.name, "my-model");
        assert_eq!(
            config.model.base_url.as_deref(),
            Some("http://localhost:8000/v1")
        );
        assert_eq!(config.model.max_tokens, Some(54272));
        assert_eq!(config.model_reference(), "my_ollama/my-model");
    }

    #[test]
    fn the_schema_names_every_section_it_reads_and_reports_the_rest() {
        let doc = document("model: {provider: x}\nmcp_servers: {a: {enable: true}}\nbogus: 1\n");
        assert_eq!(
            doc.unknown_keys(&Config::schema()),
            [
                "Unrecognised config field `mcp_servers.a.enable` - check spelling or update sven",
                "Unrecognised config field `.bogus` - check spelling or update sven",
            ]
        );
    }
}
