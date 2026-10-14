// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! When a skill, subagent or knowledge document applies to a session.
//!
//! An instruction file may carry an `activation` block in its `sven:`
//! frontmatter:
//!
//! ```yaml
//! sven:
//!   activation:
//!     paths: ["src/auth/**"]   # a file under these globs was touched or named
//!     modes: [agent, plan]     # the session runs in one of these modes
//!     states: [Executing]      # the machine is in one of these states
//!     always: false            # listed from the start of every session
//! ```
//!
//! The conditions are alternatives: the instruction applies when any one holds.
//! A file without an `activation` block always applies, which is how every
//! instruction behaved before activation existed. Evaluation is a pure
//! function of [`ActivationFacts`]; activation decides what the model is
//! *told about*, never what it is *allowed to do*.

use serde::{Deserialize, Serialize};

/// The conditions under which an instruction applies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    /// Applies in every session.
    #[serde(default)]
    pub always: bool,
    /// Applies once a file matching one of these globs (relative to the
    /// project root) has been touched or read, or the task names one.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Applies in these agent modes (`agent`, `plan`, ...), case-insensitive.
    #[serde(default)]
    pub modes: Vec<String>,
    /// Applies while the machine is in one of these states, case-insensitive.
    #[serde(default)]
    pub states: Vec<String>,
}

/// The `sven:` frontmatter block of a subagent or knowledge document. Only
/// `activation` is read; other keys are the file's own business.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SvenInstructionBlock {
    #[serde(default)]
    pub(crate) activation: Option<Activation>,
}

impl SvenInstructionBlock {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.activation
            .as_ref()
            .map_or(Ok(()), Activation::validate)
    }
}

/// What a session knows when it asks whether an instruction applies.
#[derive(Debug, Clone, Copy, Default)]
pub struct ActivationFacts<'a> {
    /// The agent mode of the session.
    pub mode: Option<&'a str>,
    /// The label of the state the machine is in.
    pub state: Option<&'a str>,
    /// Project-relative paths the session touched, read, or the task names.
    pub paths: &'a [String],
}

impl Activation {
    /// An activation that names no condition cannot ever hold; it is a
    /// mistake in the file, not a way to switch an instruction off.
    ///
    /// # Errors
    ///
    /// Returns a description when no condition is named.
    pub fn validate(&self) -> Result<(), String> {
        if self.always
            || !self.paths.is_empty()
            || !self.modes.is_empty()
            || !self.states.is_empty()
        {
            Ok(())
        } else {
            Err("activation names no condition (expected one of `always`, `paths`, `modes`, `states`)"
                .to_string())
        }
    }

    /// `true` when the instruction applies given `facts`.
    #[must_use]
    pub fn is_met(&self, facts: &ActivationFacts<'_>) -> bool {
        self.always
            || facts
                .mode
                .is_some_and(|m| self.modes.iter().any(|x| x.eq_ignore_ascii_case(m)))
            || facts
                .state
                .is_some_and(|s| self.states.iter().any(|x| x.eq_ignore_ascii_case(s)))
            || facts
                .paths
                .iter()
                .any(|p| self.paths.iter().any(|g| glob_match(g, p)))
    }

    /// `true` when the instruction is not listed from the start of every
    /// session: it waits for a mode, a state or a path.
    #[must_use]
    pub fn is_conditional(&self) -> bool {
        !self.always
    }
}

/// Whether `activation` (absent means always) holds for `facts`.
#[must_use]
pub fn is_active(activation: Option<&Activation>, facts: &ActivationFacts<'_>) -> bool {
    activation.is_none_or(|a| a.is_met(facts))
}

/// Matches `path` against `pattern`: `*` is any run of characters within one
/// path segment, `?` one character of a segment, and a `**` segment any number
/// of whole segments (including none).
#[must_use]
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.trim_start_matches("./").split('/').collect();
    let path: Vec<&str> = path.trim_start_matches("./").split('/').collect();
    segments_match(&pattern, &path)
}

fn segments_match(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments_match(rest, &path[skip..])),
        Some((segment, rest)) => path
            .split_first()
            .is_some_and(|(head, tail)| segment_match(segment, head) && segments_match(rest, tail)),
    }
}

fn segment_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut resume: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            resume = Some((p, t));
            p += 1;
        } else if let Some((star, at)) = resume {
            p = star + 1;
            t = at + 1;
            resume = Some((star, at + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn globs_follow_path_segments() {
        assert!(glob_match("src/auth/**", "src/auth/login.rs"));
        assert!(glob_match("src/auth/**", "src/auth/deep/er/x.rs"));
        assert!(!glob_match("src/auth/**", "src/billing/login.rs"));
        assert!(glob_match("**/*.rs", "a/b/c.rs"));
        assert!(glob_match("**/*.rs", "c.rs"));
        assert!(glob_match("docs/*.md", "docs/a.md"));
        assert!(!glob_match("docs/*.md", "docs/sub/a.md"));
        assert!(glob_match("./src/a?.rs", "src/ab.rs"));
        assert!(!glob_match("src/a?.rs", "src/abc.rs"));
    }

    #[test]
    fn no_activation_block_always_applies() {
        assert!(is_active(None, &ActivationFacts::default()));
    }

    #[test]
    fn conditions_are_alternatives() {
        let a = Activation {
            paths: paths(&["src/auth/**"]),
            modes: paths(&["plan"]),
            states: paths(&["Executing"]),
            ..Activation::default()
        };
        let none = ActivationFacts::default();
        assert!(!a.is_met(&none));
        assert!(a.is_met(&ActivationFacts {
            mode: Some("Plan"),
            ..none
        }));
        assert!(a.is_met(&ActivationFacts {
            state: Some("executing"),
            ..none
        }));
        let touched = paths(&["src/auth/x.rs"]);
        assert!(a.is_met(&ActivationFacts {
            paths: &touched,
            ..none
        }));
        let other = paths(&["src/billing/x.rs"]);
        assert!(!a.is_met(&ActivationFacts {
            paths: &other,
            ..none
        }));
    }

    #[test]
    fn always_applies_to_empty_facts() {
        let a = Activation {
            always: true,
            ..Activation::default()
        };
        assert!(a.is_met(&ActivationFacts::default()));
    }

    #[test]
    fn an_activation_without_conditions_is_rejected() {
        assert!(Activation::default().validate().is_err());
    }

    #[test]
    fn an_unknown_key_is_rejected_and_named() {
        let err = serde_yaml::from_str::<Activation>("pathz: [a]").unwrap_err();
        assert!(err.to_string().contains("pathz"), "{err}");
    }
}
