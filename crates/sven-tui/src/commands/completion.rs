// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Fuzzy completion matching and the CompletionManager.
//!
//! The completion manager bridges the parser output, the command registry,
//! and the completion overlay widget.  It handles:
//!
//! - Completing command names when the user types `/partial`
//! - Delegating argument completion to individual commands
//! - Fuzzy filtering and ranking of results

use std::sync::Arc;

use super::{CommandContext, ParsedCommand, CommandRegistry};

// ── Public types ──────────────────────────────────────────────────────────────

/// A single item in the completion list.
#[derive(Debug, Clone)]
pub struct CompletionItem {
    /// The value to insert when this item is selected (e.g. `"anthropic/claude-opus-4-6"`).
    pub value: String,

    /// Human-readable label shown in the overlay (may include description).
    /// If empty, `value` is used directly.
    pub display: String,

    /// Optional secondary description shown in muted style.
    pub description: Option<String>,

    /// Fuzzy match score — higher is better.  Used for sorting.
    pub score: usize,
}

impl CompletionItem {
    /// Create a simple item where value and display are the same.
    ///
    /// Used in tests and by stub command implementations.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn simple(value: impl Into<String>) -> Self {
        let v = value.into();
        Self { display: v.clone(), value: v, description: None, score: 0 }
    }

    /// Create an item with value, display, and a secondary description.
    pub fn with_desc(
        value: impl Into<String>,
        display: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            value: value.into(),
            display: display.into(),
            description: Some(description.into()),
            score: 0,
        }
    }
}

// ── Fuzzy match ───────────────────────────────────────────────────────────────

/// Fuzzy-match `pattern` against `candidate` (case-insensitive).
///
/// Returns `Some(score)` if all pattern chars appear in order in the
/// candidate, or `None` if the pattern does not match.
///
/// Scoring:
/// - +1 per matched character
/// - +5 bonus when the match starts at position 0
/// - +3 bonus for each consecutive character match
pub fn fuzzy_score(pattern: &str, candidate: &str) -> Option<usize> {
    if pattern.is_empty() {
        return Some(0);
    }

    let pattern_lc: Vec<char> = pattern.to_lowercase().chars().collect();
    let candidate_lc: Vec<char> = candidate.to_lowercase().chars().collect();

    let mut score = 0usize;
    let mut cand_idx = 0usize;
    let mut prev_matched = false;
    let mut first_match_idx: Option<usize> = None;

    for pat_ch in &pattern_lc {
        let found = candidate_lc[cand_idx..].iter().position(|c| c == pat_ch);
        match found {
            Some(offset) => {
                let actual_idx = cand_idx + offset;
                score += 1;
                if first_match_idx.is_none() {
                    first_match_idx = Some(actual_idx);
                }
                // Consecutive bonus
                if prev_matched && offset == 0 {
                    score += 3;
                }
                // Start-of-string bonus
                if actual_idx == 0 {
                    score += 5;
                }
                // Word-boundary bonus (preceded by '/', '-', '_', ' ')
                if actual_idx > 0 {
                    let prev = candidate_lc[actual_idx - 1];
                    if matches!(prev, '/' | '-' | '_' | ' ') {
                        score += 2;
                    }
                }
                cand_idx = actual_idx + 1;
                prev_matched = offset == 0;
            }
            None => return None,
        }
    }

    Some(score)
}

/// Filter and rank `items` against `filter`, returning only those that match.
///
/// Items are sorted by descending score, then alphabetically by value.
/// When `filter` is empty, all items are returned in alphabetical order.
pub fn filter_and_rank(items: Vec<CompletionItem>, filter: &str) -> Vec<CompletionItem> {
    if filter.is_empty() {
        let mut result = items;
        result.sort_by(|a, b| a.value.cmp(&b.value));
        return result;
    }

    let mut scored: Vec<CompletionItem> = items
        .into_iter()
        .filter_map(|mut item| {
            // Score against both value and display
            let score_value = fuzzy_score(filter, &item.value).unwrap_or(0);
            let score_display = fuzzy_score(filter, &item.display).unwrap_or(0);
            let score = score_value.max(score_display);
            if score > 0 || fuzzy_score(filter, &item.value).is_some() {
                item.score = score;
                Some(item)
            } else {
                None
            }
        })
        .collect();

    // Re-filter: only keep actual matches
    scored.retain(|item| {
        fuzzy_score(filter, &item.value).is_some()
            || fuzzy_score(filter, &item.display).is_some()
    });

    scored.sort_by(|a, b| b.score.cmp(&a.score).then(a.value.cmp(&b.value)));
    scored
}

// ── Completion manager ────────────────────────────────────────────────────────

/// Manages completion generation for the active input.
///
/// Bridges the parser, command registry, and completion overlay.
pub struct CompletionManager {
    registry: Arc<CommandRegistry>,
}

impl CompletionManager {
    pub fn new(registry: Arc<CommandRegistry>) -> Self {
        Self { registry }
    }

    /// Generate completions for `parsed` in `ctx`.
    ///
    /// Returns an empty vec when there is nothing to complete.
    pub fn get_completions(
        &self,
        parsed: &ParsedCommand,
        ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        match parsed {
            ParsedCommand::NotCommand | ParsedCommand::Complete { .. } => vec![],

            ParsedCommand::PartialCommand { partial } => {
                // Complete command names
                let items: Vec<CompletionItem> = self
                    .registry
                    .iter()
                    .map(|cmd| {
                        CompletionItem::with_desc(
                            cmd.name(),
                            format!("/{}", cmd.name()),
                            cmd.description(),
                        )
                    })
                    .collect();
                filter_and_rank(items, partial)
            }

            ParsedCommand::CompletingArgs { command, arg_index, partial } => {
                match self.registry.get(command) {
                    Some(cmd) => {
                        // The command is responsible for filtering and ranking
                        // its own completions (it may also pin items at specific
                        // positions, e.g. the current model at index 0).
                        // Do NOT re-rank here: a second filter_and_rank with an
                        // empty partial would alphabetically sort away pinned items.
                        cmd.complete(*arg_index, partial, ctx)
                    }
                    None => vec![],
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_score_exact_match_is_high() {
        let score = fuzzy_score("model", "model").unwrap();
        assert!(score > 10, "exact match should score high: {score}");
    }

    #[test]
    fn fuzzy_score_prefix_matches() {
        assert!(fuzzy_score("mod", "model").is_some());
        assert!(fuzzy_score("mod", "mode").is_some());
    }

    #[test]
    fn fuzzy_score_no_match_returns_none() {
        assert!(fuzzy_score("xyz", "model").is_none());
        assert!(fuzzy_score("zzz", "mode").is_none());
    }

    #[test]
    fn fuzzy_score_empty_pattern_always_matches() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn fuzzy_score_case_insensitive() {
        assert!(fuzzy_score("MOD", "model").is_some());
        assert!(fuzzy_score("mod", "MODEL").is_some());
    }

    #[test]
    fn fuzzy_score_consecutive_bonus() {
        let consec = fuzzy_score("mo", "mode").unwrap();
        let spread = fuzzy_score("me", "model").unwrap();
        // "mo" in "mode" is consecutive from start; "me" spans chars — consec should score higher
        assert!(consec >= spread, "consecutive match should score at least as high");
    }

    #[test]
    fn filter_and_rank_empty_filter_returns_all_sorted() {
        let items = vec![
            CompletionItem::simple("quit"),
            CompletionItem::simple("mode"),
            CompletionItem::simple("model"),
        ];
        let result = filter_and_rank(items, "");
        assert_eq!(result[0].value, "mode");
        assert_eq!(result[1].value, "model");
        assert_eq!(result[2].value, "quit");
    }

    #[test]
    fn filter_and_rank_filters_non_matches() {
        let items = vec![
            CompletionItem::simple("model"),
            CompletionItem::simple("mode"),
            CompletionItem::simple("quit"),
        ];
        let result = filter_and_rank(items, "mod");
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|i| i.value.starts_with("mod")));
    }

    #[test]
    fn filter_and_rank_sorts_by_score_descending() {
        let items = vec![
            CompletionItem::simple("anthropic/claude-opus"),
            CompletionItem::simple("mode"),
        ];
        // "mod" matches "mode" much better than "anthropic/claude-opus"
        let result = filter_and_rank(items, "mode");
        assert_eq!(result[0].value, "mode");
    }

    /// Regression: CompletionManager must NOT re-rank arg completions.
    /// A second filter_and_rank with empty partial alphabetically sorts items,
    /// displacing pinned entries (e.g. the current model at index 0).
    #[test]
    fn get_completions_preserves_cmd_complete_ordering() {
        use std::sync::Arc;
        use crate::commands::{CommandContext, CommandRegistry, ParsedCommand};
        use sven_config::Config;

        let registry = Arc::new(CommandRegistry::with_builtins());
        let manager = CompletionManager::new(registry);

        // Simulate "/model " (empty partial) — the current model should be first.
        let parsed = ParsedCommand::CompletingArgs {
            command: "model".to_string(),
            arg_index: 0,
            partial: "".to_string(),
        };
        let ctx = CommandContext {
            config: Arc::new(Config::default()),
            current_model_provider: "openai".into(),
            current_model_name: "gpt-4o".into(),
        };
        let items = manager.get_completions(&parsed, &ctx);
        assert!(!items.is_empty(), "must return completions for /model");
        assert_eq!(
            items[0].value, "openai/gpt-4o",
            "current model must be the first completion item, got: {:?}",
            items.iter().take(3).map(|i| &i.value).collect::<Vec<_>>()
        );
        assert!(
            items[0].display.contains("current"),
            "first item display must mention (current)"
        );
    }
}
