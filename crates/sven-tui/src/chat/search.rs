// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! In-pane search state: query, regex, match list and current-match tracking.

use crate::markdown::StyledLines;

/// All state needed to track an active text search across rendered chat lines.
#[derive(Debug, Default)]
pub struct SearchState {
    pub active: bool,
    pub query: String,
    pub matches: Vec<usize>,
    pub current: usize,
    /// Compiled regex (when the query is valid regex syntax).
    pub regex: Option<regex::Regex>,
}

impl SearchState {
    /// Recompute the list of matching line indices against the given rendered
    /// lines.  The regex is rebuilt whenever `query` changes.
    pub fn update_matches(&mut self, lines: &StyledLines) {
        if self.query.is_empty() {
            self.matches.clear();
            self.regex = None;
            return;
        }

        let re = regex::Regex::new(&format!("(?i){}", &self.query)).ok();
        self.regex = re.clone();

        self.matches = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                if let Some(re) = &re {
                    l.spans.iter().any(|s| re.is_match(&s.content))
                } else {
                    let q = self.query.to_lowercase();
                    l.spans
                        .iter()
                        .any(|s| s.content.to_lowercase().contains(&q))
                }
            })
            .map(|(i, _)| i)
            .collect();

        if self.current >= self.matches.len() {
            self.current = 0;
        }
    }

    /// The line index of the current match, or `None` when there are none.
    pub fn current_line(&self) -> Option<usize> {
        self.matches.get(self.current).copied()
    }
}
