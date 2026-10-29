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
mod context;
mod cost;
mod delegation;
mod disclosure;
mod listing;
mod ocr;
mod tools;

use std::path::Path;

use serde::{Deserialize, Serialize};
use sven_config::{ConfigDocument, Schema};
use sven_decision::config::DecisionConfig;
use sven_mcp_client::{mcp_servers_schema, McpServers};
use sven_model_drivers::{providers_schema, ModelConfig, Providers};

pub use agent::AgentConfig;
pub use context::{AssemblyConfig, AssemblyMode, ContextSettings};
pub use cost::CostSettings;
pub use delegation::{DelegationConfig, DelegationPolicy};
pub use disclosure::DisclosureConfig;
pub use listing::ListingConfig;
pub use ocr::{OcrConfig, Specs};
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
    /// The decision model that advises the harness; off unless the file
    /// enables it. See `docs/internals/decisions.md`.
    pub decision: DecisionConfig,
    /// How the context of a request is assembled from the conversation. See
    /// `docs/internals/context-assembly.md`.
    pub context: ContextSettings,
    /// Whether a rewrite of the prompt is weighed against the provider's
    /// prompt cache, and the prices that uses. See
    /// `docs/internals/cost-model.md`.
    pub cost: CostSettings,
    /// Whether `task` declines a sub-agent that would cost more than it
    /// saves. See `docs/concepts/delegation.md`.
    pub delegation: DelegationConfig,
    /// How much of each skill the system prompt carries. See
    /// `docs/internals/instruction-disclosure.md`.
    pub skills: ListingConfig,
    /// How much of each subagent the system prompt carries.
    pub subagents: ListingConfig,
    /// How much of each knowledge document the system prompt carries.
    pub knowledge: ListingConfig,
    /// The OCR model `document_convert` uses and how it is run. See
    /// `docs/guides/document-conversion.md`.
    pub ocr: OcrConfig,
}

impl Config {
    /// The sven service a key has been provided for: a `providers.sven`
    /// entry with a key, or the server `sven auth login` signed in to. Where
    /// `web_search` searches unless `sven rc` signed in to another one (see
    /// `WebSearchConfig::backend`).
    #[must_use]
    pub fn sven_search_endpoint(&self) -> Option<sven_tools_web::SvenEndpoint> {
        self.sven_search_endpoint_with(sven_model_drivers::provider_key)
    }

    /// [`Self::sven_search_endpoint`], with `key_of` finding an entry's key.
    fn sven_search_endpoint_with(
        &self,
        key_of: impl Fn(&sven_model_drivers::ProviderEntry) -> Option<String>,
    ) -> Option<sven_tools_web::SvenEndpoint> {
        let entry = self.providers.get("sven").cloned().unwrap_or_else(|| {
            sven_model_drivers::ProviderEntry {
                name: "sven".into(),
                ..Default::default()
            }
        });
        let base_url = sven_model_drivers::provider_base_url(&entry)?;
        Some(sven_tools_web::SvenEndpoint {
            base_url,
            api_key: key_of(&entry)?,
        })
    }

    /// The `web_search` tool this configuration gives a session.
    #[must_use]
    pub fn web_search_tool(&self) -> sven_tools_web::WebSearchTool {
        self.tools.web.search_tool(self.sven_search_endpoint())
    }

    /// The keys of the sections this type reads.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::fields([
            ("model", ModelConfig::schema()),
            ("agent", AgentConfig::schema()),
            ("tools", ToolsConfig::schema()),
            ("providers", providers_schema()),
            ("mcp_servers", mcp_servers_schema()),
            ("decision", DecisionConfig::schema()),
            ("context", ContextSettings::schema()),
            ("cost", CostSettings::schema()),
            ("delegation", DelegationConfig::schema()),
            ("skills", ListingConfig::schema()),
            ("subagents", ListingConfig::schema()),
            ("knowledge", ListingConfig::schema()),
            ("ocr", OcrConfig::schema()),
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

    /// How much of each skill, subagent and knowledge entry the system prompt
    /// carries.
    #[must_use]
    pub fn instruction_disclosure(&self) -> sven_turn::instructions::InstructionDisclosure {
        sven_turn::instructions::InstructionDisclosure {
            skills: self.skills.disclosure,
            subagents: self.subagents.disclosure,
            knowledge: self.knowledge.disclosure,
        }
    }

    /// The configuration `document` holds, settled ([`Self::settle`]).
    #[must_use]
    pub fn from_document(document: &ConfigDocument) -> Self {
        let mut config: Self = document.decode();
        config.settle_from(document);
        config
    }

    /// Settles what only the file the configuration was read from can say:
    /// the active model ([`Self::settle`]), and the defaults of a program
    /// that reads the file.
    ///
    /// Such a program discloses tools on demand, and lists skills, subagents
    /// and knowledge documents by a short index, unless the file says
    /// otherwise. `Config::default()` - what an embedding application builds
    /// in code - stays `full`, so the tools such an application registers are
    /// in every request as before.
    pub fn settle_from(&mut self, document: &ConfigDocument) {
        self.settle(document.contains("model"));
        if !document.contains_path(&["tools", "disclosure", "mode"]) {
            self.tools.disclosure.mode = sven_tool_registry::Disclosure::Deferred;
        }
        for (section, listing) in [
            ("skills", &mut self.skills),
            ("subagents", &mut self.subagents),
            ("knowledge", &mut self.knowledge),
        ] {
            if !document.contains_path(&[section, "disclosure"]) {
                listing.disclosure = sven_turn::instructions::ListingDisclosure::Deferred;
            }
        }
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
    /// "A sven API key has been provided": a `providers.sven` entry with its
    /// base URL and a key makes it the search service; without a key, or
    /// without the entry, there is none.
    #[test]
    fn a_sven_provider_with_a_key_is_the_search_service() {
        let no_key = |_: &sven_model_drivers::ProviderEntry| None;
        let stored = |entry: &sven_model_drivers::ProviderEntry| {
            (entry.name == "sven" && entry.api_key.is_none()).then(|| "signed-in".to_string())
        };
        let configured = |entry: &sven_model_drivers::ProviderEntry| entry.api_key.clone();
        let mut cfg = Config::default();
        assert_eq!(
            cfg.sven_search_endpoint_with(no_key),
            None,
            "no key, no search service"
        );
        // Signed in to the hosted service (`sven auth login` stored its key).
        assert_eq!(
            cfg.sven_search_endpoint_with(stored),
            Some(sven_tools_web::SvenEndpoint {
                base_url: "https://sven.swedishembedded.com/v1".into(),
                api_key: "signed-in".into(),
            })
        );
        // A `providers.sven` entry names another service and its key.
        cfg.providers.insert(
            "sven".into(),
            sven_model_drivers::ProviderEntry {
                name: "openai".into(),
                base_url: Some("https://sven.example/v1".into()),
                api_key: Some("service_key".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            cfg.sven_search_endpoint_with(configured),
            Some(sven_tools_web::SvenEndpoint {
                base_url: "https://sven.example/v1".into(),
                api_key: "service_key".into(),
            })
        );
    }

    use super::*;

    fn document(yaml: &str) -> ConfigDocument {
        ConfigDocument::from_yaml(yaml).unwrap()
    }

    #[test]
    fn a_program_that_reads_the_file_defers_the_catalogs_unless_the_file_says_otherwise() {
        use sven_turn::instructions::{InstructionDisclosure, ListingDisclosure::*};
        let all = |mode| InstructionDisclosure {
            skills: mode,
            subagents: mode,
            knowledge: mode,
        };
        assert_eq!(Config::default().instruction_disclosure(), all(Full));
        let silent = Config::from_document(&document("agent: {max_tool_rounds: 7}\n"));
        assert_eq!(silent.instruction_disclosure(), all(Deferred));
        let mixed = Config::from_document(&document(
            "skills: {disclosure: full}\nknowledge: {disclosure: deferred}\n",
        ));
        assert_eq!(
            mixed.instruction_disclosure(),
            InstructionDisclosure {
                skills: Full,
                subagents: Deferred,
                knowledge: Deferred,
            }
        );
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
    fn a_file_that_does_not_say_discloses_tools_on_demand_and_a_default_config_does_not() {
        use sven_tool_registry::Disclosure;
        assert_eq!(Config::default().tools.disclosure.mode, Disclosure::Full);
        assert_eq!(
            Config::from_document(&document("agent: {max_tool_rounds: 7}\n"))
                .tools
                .disclosure
                .mode,
            Disclosure::Deferred
        );
        assert_eq!(
            Config::from_document(&document("tools: {disclosure: {mode: full}}\n"))
                .tools
                .disclosure
                .mode,
            Disclosure::Full
        );
    }

    #[test]
    fn a_section_the_file_leaves_partial_does_not_discard_the_rest() {
        let config = Config::from_document(&document(
            "model: {provider: anthropic, name: claude-opus-4-6}\n\
             agent: {compaction_summary_tokens: 3000}\n\
             tools: {timeout_secs: 99}\n",
        ));
        assert_eq!(config.model.provider, "anthropic");
        assert_eq!(config.agent.compaction_summary_tokens, 3000);
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

    /// A removed key is reported with what replaces it, where the key is
    /// read - not left for a later "no API key" with no hint.
    #[test]
    fn a_removed_key_is_reported_with_its_replacement() {
        let doc = document("providers: {gw: {name: openai, api_key_env: TOKEN}}\n");
        let warnings = doc.unknown_keys(&Config::schema());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("providers.gw.api_key_env")
                && warnings[0].contains("api_key_file"),
            "{warnings:?}"
        );
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
