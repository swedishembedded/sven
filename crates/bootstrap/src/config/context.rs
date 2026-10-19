// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The `context` section: how the context of a request is assembled from the
//! conversation. See `docs/internals/context-assembly.md`.

use serde::{Deserialize, Serialize};
use sven_chunks::StoreLimits;
use sven_config::Schema;
use sven_turn::assembly::{
    PolicyConfig, DEFAULT_BUDGET_TOKENS, DEFAULT_KEEP_RECENT_ROUNDS, DEFAULT_MIN_DEMOTE_TOKENS,
};

/// `context`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextSettings {
    pub assembly: AssemblyConfig,
    /// The chunk store of the session that delegated to this one, read-only
    /// (`sven acp serve --evidence-store`). Set by the parent that starts a
    /// sub-agent; never read from or written to the configuration file.
    #[serde(skip)]
    pub evidence_store: Option<std::path::PathBuf>,
}

impl ContextSettings {
    /// The keys of the `context` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::fields([("assembly", AssemblyConfig::schema())])
    }
}

/// How the history becomes the context of a request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyMode {
    /// The conversation is sent as it is.
    #[default]
    Off,
    /// Tool results of older rounds are sent in a smaller form, under a token
    /// budget, and can be expanded back to their raw text.
    Demote,
    /// `demote`, and a resumed session starts from a rebuilt history: what
    /// the user said, plus a recap of the stored tool activity, instead of
    /// the whole transcript replayed.
    Reconstruct,
}

/// `context.assembly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AssemblyConfig {
    /// `off` sends the conversation unchanged; `demote` shrinks the tool
    /// results of older rounds once the conversation exceeds `budget_tokens`;
    /// `reconstruct` also rebuilds the history of a resumed session.
    pub mode: AssemblyMode,
    /// Estimated tokens of conversation above which results are demoted.
    pub budget_tokens: u32,
    /// Rounds of tool calls, counted back from the newest, whose results are
    /// always sent in full.
    pub keep_recent_rounds: u32,
    /// Results smaller than this many estimated tokens are never demoted.
    pub min_demote_tokens: u32,
    /// Bytes of evidence held in memory; the rest is read back from disk.
    pub resident_bytes: usize,
}

impl Default for AssemblyConfig {
    fn default() -> Self {
        Self {
            mode: AssemblyMode::default(),
            budget_tokens: DEFAULT_BUDGET_TOKENS,
            keep_recent_rounds: DEFAULT_KEEP_RECENT_ROUNDS,
            min_demote_tokens: DEFAULT_MIN_DEMOTE_TOKENS,
            resident_bytes: StoreLimits::default().max_resident_bytes,
        }
    }
}

impl AssemblyConfig {
    /// The keys of the `context.assembly` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&[
            "mode",
            "budget_tokens",
            "keep_recent_rounds",
            "min_demote_tokens",
            "resident_bytes",
        ])
    }

    /// Whether any assembly is on.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.mode != AssemblyMode::Off
    }

    /// Whether a resumed session starts from a rebuilt history.
    #[must_use]
    pub fn reconstructs(&self) -> bool {
        self.mode == AssemblyMode::Reconstruct
    }

    /// The policy these settings describe.
    #[must_use]
    pub fn policy(&self) -> PolicyConfig {
        PolicyConfig {
            budget_tokens: self.budget_tokens,
            keep_recent_rounds: self.keep_recent_rounds,
            min_demote_tokens: self.min_demote_tokens,
        }
    }

    /// The bounds of the chunk store these settings describe.
    #[must_use]
    pub fn store_limits(&self) -> StoreLimits {
        StoreLimits {
            max_resident_bytes: self.resident_bytes,
            ..StoreLimits::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use sven_config::ConfigDocument;

    use super::*;
    use crate::config::Config;

    fn document(yaml: &str) -> ConfigDocument {
        ConfigDocument::from_yaml(yaml).unwrap()
    }

    #[test]
    fn assembly_is_off_unless_the_file_turns_it_on() {
        let config = Config::from_document(&document("agent: {max_tool_rounds: 7}\n"));
        assert_eq!(config.context.assembly.mode, AssemblyMode::Off);
        assert!(!config.context.assembly.enabled());
        assert_eq!(Config::default().context, ContextSettings::default());
    }

    #[test]
    fn the_section_sets_the_mode_and_the_policy() {
        let config = Config::from_document(&document(
            "context:\n  assembly:\n    mode: demote\n    budget_tokens: 5000\n    keep_recent_rounds: 3\n    min_demote_tokens: 32\n    resident_bytes: 1024\n",
        ));
        let a = config.context.assembly;
        assert!(a.enabled());
        assert_eq!(
            a.policy(),
            PolicyConfig {
                budget_tokens: 5000,
                keep_recent_rounds: 3,
                min_demote_tokens: 32,
            }
        );
        assert_eq!(a.store_limits().max_resident_bytes, 1024);
    }

    #[test]
    fn reconstruct_demotes_too_and_is_the_only_mode_that_rebuilds_a_history() {
        let demote = Config::from_document(&document("context: {assembly: {mode: demote}}\n"));
        assert!(demote.context.assembly.enabled() && !demote.context.assembly.reconstructs());
        let rebuild =
            Config::from_document(&document("context: {assembly: {mode: reconstruct}}\n"));
        assert!(rebuild.context.assembly.enabled() && rebuild.context.assembly.reconstructs());
    }

    #[test]
    fn the_defaults_are_the_policys_own() {
        assert_eq!(AssemblyConfig::default().policy(), PolicyConfig::default());
    }

    #[test]
    fn an_unrecognised_key_is_reported_and_the_known_ones_are_not() {
        let known = document("context: {assembly: {mode: demote, budget_tokens: 1}}\n");
        assert!(known.unknown_keys(&Config::schema()).is_empty());
        let typo = document("context: {assembly: {mod: demote}}\n");
        let unknown = typo.unknown_keys(&Config::schema());
        assert_eq!(unknown.len(), 1);
        assert!(unknown[0].contains("context.assembly.mod"), "{unknown:?}");
    }

    #[test]
    fn the_delegating_sessions_store_is_a_runtime_fact_not_a_file_key() {
        let config = Config::from_document(&document(
            "context:\n  evidence_store: /elsewhere/.sven/context/x\n",
        ));
        assert_eq!(config.context.evidence_store, None);
        let set = ContextSettings {
            evidence_store: Some("/p/.sven/context/id".into()),
            ..ContextSettings::default()
        };
        assert!(serde_json::to_value(&set)
            .unwrap()
            .get("evidence_store")
            .is_none());
    }
}
