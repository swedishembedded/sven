// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::path::{Path, PathBuf};

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

// ── RolePolicy ────────────────────────────────────────────────────────────────

/// Per-role policy overlay applied on top of the global [`ToolPolicy`].
///
/// Used by agent teams to enforce role-specific restrictions without
/// touching the global tool configuration.  For example, a `reviewer` role
/// should not be able to call `write_file`, `edit_file`, or `delete_file`.
///
/// Policy resolution order:
/// 1. Role `deny_tools` — if the tool name is in this list, always deny.
/// 2. Filesystem scope — if `fs_root` is set, file-path arguments that
///    resolve outside the root are denied.
/// 3. Fall through to the global [`ToolPolicy`].
#[derive(Debug, Clone, Default)]
pub struct RolePolicy {
    /// Tool names that are always denied for this role.
    deny_tools: Vec<String>,
    /// Optional filesystem root.  File tools with paths outside this root
    /// are denied.  Typically set to the agent's Git worktree path.
    fs_root: Option<PathBuf>,
}

impl RolePolicy {
    /// Build a role policy from a list of denied tool names and an optional
    /// filesystem root restriction.
    pub fn new(deny_tools: impl IntoIterator<Item = String>, fs_root: Option<PathBuf>) -> Self {
        Self {
            deny_tools: deny_tools.into_iter().collect(),
            fs_root,
        }
    }

    /// Return `true` if the tool name is explicitly denied for this role.
    pub fn is_tool_denied(&self, tool_name: &str) -> bool {
        self.deny_tools.iter().any(|d| {
            d == tool_name || (d.ends_with('*') && tool_name.starts_with(d.trim_end_matches('*')))
        })
    }

    /// Return `true` if the given file path is allowed under the filesystem
    /// root restriction, or if no restriction is set.
    pub fn is_path_allowed(&self, path: &Path) -> bool {
        match &self.fs_root {
            None => true,
            Some(root) => {
                // Canonicalize both to resolve symlinks and `..` traversal.
                let canonical_path =
                    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
                let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
                canonical_path.starts_with(&canonical_root)
            }
        }
    }

    /// Combine role-level denial check with the global policy.
    ///
    /// Returns `Deny` if the role denies the tool; otherwise delegates to the
    /// global `ToolPolicy`.
    pub fn decide_with_global(&self, tool_name: &str, global: &ToolPolicy) -> ApprovalPolicy {
        if self.is_tool_denied(tool_name) {
            return ApprovalPolicy::Deny;
        }
        global.decide(tool_name)
    }

    /// The filesystem root restriction, if any.
    pub fn fs_root(&self) -> Option<&Path> {
        self.fs_root.as_deref()
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

    // ── RolePolicy ────────────────────────────────────────────────────────────

    #[test]
    fn role_policy_denies_listed_tool() {
        let rp = RolePolicy::new(
            vec!["write_file".to_string(), "edit_file".to_string()],
            None,
        );
        assert!(rp.is_tool_denied("write_file"));
        assert!(rp.is_tool_denied("edit_file"));
        assert!(!rp.is_tool_denied("read_file"));
    }

    #[test]
    fn role_policy_path_allowed_without_restriction() {
        let rp = RolePolicy::default();
        assert!(rp.is_path_allowed(Path::new("/any/path")));
    }

    #[test]
    fn role_policy_path_allowed_within_root() {
        let rp = RolePolicy::new(Vec::new(), Some(PathBuf::from("/repo/worktree")));
        assert!(rp.is_path_allowed(Path::new("/repo/worktree/src/main.rs")));
    }

    #[test]
    fn role_policy_path_denied_outside_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        // Create a real path outside root
        let outside = std::env::temp_dir().join("outside_file.txt");
        std::fs::write(&outside, "x").unwrap();

        let rp = RolePolicy::new(Vec::new(), Some(root));
        assert!(!rp.is_path_allowed(&outside));
    }

    #[test]
    fn role_policy_decide_with_global_deny_overrides() {
        let global = policy_with(&["read_file"], &[]);
        let rp = RolePolicy::new(vec!["read_file".to_string()], None);
        // Role denies read_file even though global would auto-approve
        assert_eq!(
            rp.decide_with_global("read_file", &global),
            ApprovalPolicy::Deny
        );
    }

    #[test]
    fn role_policy_decide_falls_through_to_global() {
        let global = policy_with(&["read_file"], &[]);
        let rp = RolePolicy::new(vec!["write_file".to_string()], None);
        // read_file not in deny list → global auto-approve
        assert_eq!(
            rp.decide_with_global("read_file", &global),
            ApprovalPolicy::Auto
        );
    }

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

    // ── Adversarial policy inputs ─────────────────────────────────────────────

    #[test]
    fn adversarial_glob_with_invalid_regex_char_does_not_panic() {
        // `*[` is not a valid regex after glob-to-regex expansion.
        // glob_to_regex should return None, and no tool should match.
        let p = policy_with(&["*["], &[]);
        // Must not panic; policy falls back to Ask when no valid patterns exist.
        let _ = p.decide("any command");
    }

    #[test]
    fn adversarial_empty_pattern_does_not_match_everything() {
        let p = policy_with(&[""], &[]);
        // `^$` only matches the empty string, not arbitrary commands.
        assert_ne!(p.decide("rm -rf /"), ApprovalPolicy::Auto);
        assert_ne!(p.decide("cat /etc/passwd"), ApprovalPolicy::Auto);
    }

    #[test]
    fn adversarial_tab_separator_bypasses_space_glob() {
        // Pattern `rm *` should only match `rm ` + anything with a space separator.
        // A command using a tab instead of a space should NOT match.
        let p = policy_with(&["rm *"], &[]);
        let result = p.decide("rm\t-rf /");
        assert_ne!(
            result,
            ApprovalPolicy::Auto,
            "tab should not match space glob"
        );
    }

    #[test]
    fn adversarial_uppercase_command_does_not_match_lowercase_pattern() {
        let p = policy_with(&["rm"], &[]);
        // Pattern is case-sensitive; `RM` should not match `rm`.
        assert_ne!(p.decide("RM /tmp/foo"), ApprovalPolicy::Auto);
    }

    #[test]
    fn adversarial_unicode_homoglyph_does_not_match_ascii_pattern() {
        let p = policy_with(&["rm"], &[]);
        // Cyrillic р (U+0440) looks like Latin r but is a different codepoint.
        assert_ne!(p.decide("рm /tmp/foo"), ApprovalPolicy::Auto);
    }

    #[test]
    fn adversarial_very_long_command_string_does_not_hang() {
        let p = policy_with(&["cat *"], &["rm *"]);
        let long_cmd = format!("cat {}", "A".repeat(1_000_000));
        // Must complete without hanging (no catastrophic backtracking).
        let _ = p.decide(&long_cmd);
    }

    #[test]
    fn adversarial_deny_pattern_with_invalid_glob_does_not_panic() {
        let p = policy_with(&[], &["*["]);
        let _ = p.decide("rm -rf /");
    }

    #[test]
    fn adversarial_role_policy_wildcard_denies_prefix() {
        let rp = RolePolicy::new(vec!["file_*".to_string()], None);
        assert!(rp.is_tool_denied("file_write"));
        assert!(rp.is_tool_denied("file_read"));
        assert!(!rp.is_tool_denied("shell"));
    }

    #[test]
    fn adversarial_path_with_dotdot_is_denied_when_outside_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        // /tmp/<root>/../../etc/passwd resolves outside root
        let outside = root.join("../../etc/passwd");
        let rp = RolePolicy::new(Vec::new(), Some(root));
        assert!(!rp.is_path_allowed(&outside));
    }

    #[test]
    fn adversarial_path_allowed_with_no_fs_root() {
        let rp = RolePolicy::default();
        assert!(rp.is_path_allowed(Path::new("/etc/passwd")));
        assert!(rp.is_path_allowed(Path::new("/root/.ssh/id_rsa")));
    }
}
