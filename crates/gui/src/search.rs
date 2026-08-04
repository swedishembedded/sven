// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! In-chat search state.

use std::sync::{Arc, Mutex};

/// Per-session search state.
#[derive(Default)]
pub struct SearchState {
    /// Current search query (empty = no active search).
    pub query: String,
    /// Indices of matching message rows in the committed messages model.
    pub match_indices: Vec<usize>,
    /// Currently highlighted match (index into `match_indices`).
    pub current: usize,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update matches for a new query against the given message contents.
    pub fn update(&mut self, query: &str, messages: &[String]) {
        self.query = query.to_lowercase();
        self.match_indices = if self.query.is_empty() {
            vec![]
        } else {
            messages
                .iter()
                .enumerate()
                .filter(|(_, m)| m.to_lowercase().contains(&self.query))
                .map(|(i, _)| i)
                .collect()
        };
        self.current = 0;
    }

    /// Advance to the next match (wraps around).
    pub fn next(&mut self) {
        if self.match_indices.is_empty() {
            return;
        }
        self.current = (self.current + 1) % self.match_indices.len();
    }

    /// Go to the previous match (wraps around).
    pub fn prev(&mut self) {
        if self.match_indices.is_empty() {
            return;
        }
        if self.current == 0 {
            self.current = self.match_indices.len() - 1;
        } else {
            self.current -= 1;
        }
    }

    /// Current focused match row index (in the message model), if any.
    pub fn current_row(&self) -> Option<usize> {
        self.match_indices.get(self.current).copied()
    }

    pub fn match_count(&self) -> usize {
        self.match_indices.len()
    }

    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }
}

pub type SharedSearch = Arc<Mutex<SearchState>>;

pub fn new_shared_search() -> SharedSearch {
    Arc::new(Mutex::new(SearchState::new()))
}
