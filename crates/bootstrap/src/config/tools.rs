// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `tools:` section of the configuration file.
//!
//! The section is assembled here, where the tools are, but each tool's own
//! settings are owned by the crate that implements it: `web` and `lints` by
//! `sven-tools-web`, `memory` by `sven-tools-ctx`, `gdb` by `sven-tools-gdb`,
//! `asr` by `sven-tools-fs` and `context` by [`crate::context_query`]. What
//! is left - the shell command patterns, the tools a session never offers and
//! the shell's timeout - is read here.
//!
//! Swedish Embedded AB implements permissioned tool sets for agents for its
//! clients. If your team needs expertise in deciding what a language-model
//! agent may run then you can procure our services by sending an email to
//! info@swedishembedded.com.

use serde::{Deserialize, Serialize};
use sven_config::Schema;
use sven_tool_registry::ToolPolicy;
use sven_tools_ctx::MemoryConfig;
use sven_tools_fs::AsrConfig;
use sven_tools_gdb::GdbConfig;
use sven_tools_web::{LintsConfig, WebConfig};

use crate::context_query::ContextConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    /// Automatically approve shell commands matching these glob patterns
    pub auto_approve_patterns: Vec<String>,
    /// Block shell commands matching these glob patterns
    pub deny_patterns: Vec<String>,
    /// Tools, by name, a session never offers the model and never runs -
    /// built-in, MCP or supplied by an embedding application alike.
    pub disabled: Vec<String>,
    /// Timeout in seconds for a single tool call
    pub timeout_secs: u64,
    /// Web fetch and search configuration
    #[serde(default)]
    pub web: WebConfig,
    /// Persistent memory configuration
    #[serde(default)]
    pub memory: MemoryConfig,
    /// Linter configuration
    #[serde(default)]
    pub lints: LintsConfig,
    /// GDB debugging configuration
    #[serde(default)]
    pub gdb: GdbConfig,
    /// Memory-mapped context tools configuration (RLM pattern)
    #[serde(default)]
    pub context: ContextConfig,
    /// Speech-to-text fallback used by the `attach_file` tool when the active
    /// model cannot accept audio natively.
    #[serde(default)]
    pub asr: AsrConfig,
}

impl ToolsConfig {
    /// The keys of the `tools:` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&[
            "auto_approve_patterns",
            "deny_patterns",
            "disabled",
            "timeout_secs",
        ])
        .with("web", WebConfig::schema())
        .with("memory", MemoryConfig::schema())
        .with("lints", LintsConfig::schema())
        .with("gdb", GdbConfig::schema())
        .with("asr", AsrConfig::schema())
        .with("context", ContextConfig::schema())
    }

    /// What decides whether a shell command runs, asks, or is refused.
    #[must_use]
    pub fn policy(&self) -> ToolPolicy {
        ToolPolicy::from_patterns(&self.auto_approve_patterns, &self.deny_patterns)
    }
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            auto_approve_patterns: vec![
                "cat *".into(),
                "ls *".into(),
                "find *".into(),
                "rg *".into(),
                "grep *".into(),
            ],
            deny_patterns: vec!["rm -rf /*".into(), "dd if=*".into()],
            disabled: Vec::new(),
            timeout_secs: 30,
            web: WebConfig::default(),
            memory: MemoryConfig::default(),
            lints: LintsConfig::default(),
            gdb: GdbConfig::default(),
            context: ContextConfig::default(),
            asr: AsrConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_tool_api::ApprovalPolicy;

    #[test]
    fn the_default_policy_auto_approves_reads_and_asks_for_the_rest() {
        let p = ToolsConfig::default().policy();
        assert_eq!(p.decide("cat README.md"), ApprovalPolicy::Auto);
        assert_eq!(p.decide("ls /tmp"), ApprovalPolicy::Auto);
        assert_eq!(p.decide("cargo build"), ApprovalPolicy::Ask);
        assert_eq!(p.decide("rm -rf /*"), ApprovalPolicy::Deny);
    }

    #[test]
    fn the_configured_patterns_replace_the_defaults() {
        let tools: ToolsConfig = serde_yaml::from_str(
            "auto_approve_patterns: ['make *']\ndeny_patterns: ['make clean']\n",
        )
        .unwrap();
        let p = tools.policy();
        assert_eq!(p.decide("make check"), ApprovalPolicy::Auto);
        assert_eq!(p.decide("make clean"), ApprovalPolicy::Deny);
        assert_eq!(p.decide("cat x"), ApprovalPolicy::Ask);
    }

    #[test]
    fn an_absent_section_keeps_its_defaults() {
        let tools: ToolsConfig = serde_yaml::from_str("timeout_secs: 45\n").unwrap();
        assert_eq!(tools.timeout_secs, 45);
        assert_eq!(
            tools.web.fetch_max_chars,
            WebConfig::default().fetch_max_chars
        );
        assert_eq!(tools.asr, AsrConfig::default());
        assert_eq!(tools.deny_patterns, ToolsConfig::default().deny_patterns);
    }
}
