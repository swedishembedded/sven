// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use regex::Regex;
use sven_config::ToolsConfig;

/// Per-tool approval policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Always run without asking
    Auto,
    /// Ask user before each invocation
    Ask,
    /// Never run; return an error
    Deny,
}

/// Policy engine that maps a tool call to an approval decision.
#[derive(Debug)]
pub struct ToolPolicy {
    auto_patterns: Vec<Regex>,
    deny_patterns: Vec<Regex>,
}

impl ToolPolicy {
    pub fn from_config(cfg: &ToolsConfig) -> Self {
        let compile = |patterns: &[String]| -> Vec<Regex> {
            patterns.iter().filter_map(|p| glob_to_regex(p)).collect()
        };
        Self {
            auto_patterns: compile(&cfg.auto_approve_patterns),
            deny_patterns: compile(&cfg.deny_patterns),
        }
    }

    /// Decide whether a tool call (identified by its command string) should
    /// run automatically, prompt the user, or be denied.
    pub fn decide(&self, command: &str) -> ApprovalPolicy {
        for re in &self.deny_patterns {
            if re.is_match(command) {
                return ApprovalPolicy::Deny;
            }
        }
        for re in &self.auto_patterns {
            if re.is_match(command) {
                return ApprovalPolicy::Auto;
            }
        }
        ApprovalPolicy::Ask
    }
}

/// Convert a simple shell glob pattern to a [`Regex`].
/// Only `*` (match anything) and `?` (match one char) are supported.
fn glob_to_regex(pattern: &str) -> Option<Regex> {
    let mut re = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c => {
                for esc in regex::escape(&c.to_string()).chars() {
                    re.push(esc);
                }
            }
        }
    }
    re.push('$');
    Regex::new(&re).ok()
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sven_config::ToolsConfig;

    fn policy_with(auto: &[&str], deny: &[&str]) -> ToolPolicy {
        ToolPolicy::from_config(&ToolsConfig {
            auto_approve_patterns: auto.iter().map(|s| s.to_string()).collect(),
            deny_patterns: deny.iter().map(|s| s.to_string()).collect(),
            ..ToolsConfig::default()
        })
    }

    // ── Deny takes priority ───────────────────────────────────────────────────

    #[test]
    fn deny_beats_auto_for_same_pattern() {
        let p = policy_with(&["rm *"], &["rm *"]);
        assert_eq!(p.decide("rm /tmp/foo"), ApprovalPolicy::Deny);
    }

    #[test]
    fn deny_exact_match() {
        let p = policy_with(&[], &["rm -rf /*"]);
        assert_eq!(p.decide("rm -rf /*"), ApprovalPolicy::Deny);
    }

    #[test]
    fn deny_does_not_match_different_prefix() {
        let p = policy_with(&[], &["rm -rf /*"]);
        // Completely different command → should Ask, not Deny
        assert_ne!(p.decide("git status"), ApprovalPolicy::Deny);
    }

    // ── Auto-approve ──────────────────────────────────────────────────────────

    #[test]
    fn auto_approve_wildcard_prefix() {
        let p = policy_with(&["cat *"], &[]);
        assert_eq!(p.decide("cat /etc/hosts"), ApprovalPolicy::Auto);
    }

    #[test]
    fn auto_approve_exact_command() {
        let p = policy_with(&["ls"], &[]);
        assert_eq!(p.decide("ls"), ApprovalPolicy::Auto);
    }

    #[test]
    fn auto_approve_question_mark_matches_one_char() {
        let p = policy_with(&["ls ?"], &[]);
        assert_eq!(p.decide("ls -"), ApprovalPolicy::Auto);
        // Two chars after space → no match
        assert_ne!(p.decide("ls --"), ApprovalPolicy::Auto);
    }

    // ── Ask fallback ──────────────────────────────────────────────────────────

    #[test]
    fn unknown_command_results_in_ask() {
        let p = policy_with(&["cat *"], &["rm -rf /*"]);
        assert_eq!(p.decide("git commit -m test"), ApprovalPolicy::Ask);
    }

    #[test]
    fn empty_patterns_always_ask() {
        let p = policy_with(&[], &[]);
        assert_eq!(p.decide("anything"), ApprovalPolicy::Ask);
    }

    // ── Default config ────────────────────────────────────────────────────────

    #[test]
    fn default_config_auto_approves_cat() {
        let p = ToolPolicy::from_config(&ToolsConfig::default());
        assert_eq!(p.decide("cat README.md"), ApprovalPolicy::Auto);
    }

    #[test]
    fn default_config_auto_approves_ls() {
        let p = ToolPolicy::from_config(&ToolsConfig::default());
        assert_eq!(p.decide("ls /tmp"), ApprovalPolicy::Auto);
    }

    #[test]
    fn default_config_asks_for_write_command() {
        let p = ToolPolicy::from_config(&ToolsConfig::default());
        assert_eq!(p.decide("cargo build"), ApprovalPolicy::Ask);
    }
}
