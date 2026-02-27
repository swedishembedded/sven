// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Question modal: multi-step question/answer flow triggered by the agent's
//! `AskQuestion` tool.

use sven_tools::Question;
use tokio::sync::oneshot;

/// Snapshot of a question's answer state, used to navigate back.
#[derive(Clone)]
struct AnswerState {
    selected_options: Vec<usize>,
    other_selected: bool,
    other_input: String,
    other_cursor: usize,
    focused_option: usize,
}

/// Active multi-step question/answer flow state.
pub struct QuestionModal {
    pub questions: Vec<Question>,
    /// Answers collected so far (one per completed question).
    pub answers: Vec<String>,
    pub current_q: usize,
    /// Selected option indices for the current question (empty when using "Other").
    pub selected_options: Vec<usize>,
    /// True when the "Other" option is active.
    pub other_selected: bool,
    /// Text typed into the "Other" free-text field.
    pub other_input: String,
    /// Byte cursor into `other_input`.
    pub other_cursor: usize,
    /// Index of the keyboard-focused row in the current question.
    /// Rows: 0..options.len() are regular options; options.len() is "Other".
    pub focused_option: usize,
    /// Per-question snapshots so the user can navigate back.
    snapshots: Vec<AnswerState>,
    answer_tx: oneshot::Sender<String>,
}

impl QuestionModal {
    pub fn new(questions: Vec<Question>, answer_tx: oneshot::Sender<String>) -> Self {
        Self {
            questions,
            answers: Vec::new(),
            current_q: 0,
            selected_options: Vec::new(),
            other_selected: false,
            other_input: String::new(),
            other_cursor: 0,
            focused_option: 0,
            snapshots: Vec::new(),
            answer_tx,
        }
    }

    /// Total number of rows in the current question (options + "Other").
    pub fn row_count(&self) -> usize {
        self.questions
            .get(self.current_q)
            .map(|q| q.options.len() + 1)
            .unwrap_or(1)
    }

    /// Move keyboard focus to the previous row, wrapping from the top.
    pub fn focus_prev(&mut self) {
        if self.other_selected {
            return; // locked into Other text input
        }
        let n = self.row_count();
        self.focused_option = if self.focused_option == 0 {
            n.saturating_sub(1)
        } else {
            self.focused_option - 1
        };
    }

    /// Move keyboard focus to the next row, wrapping at the bottom.
    pub fn focus_next(&mut self) {
        if self.other_selected {
            return; // locked into Other text input
        }
        let n = self.row_count();
        self.focused_option = (self.focused_option + 1) % n.max(1);
    }

    /// Select or toggle the currently focused row.
    ///
    /// If the "Other" row is focused this activates the Other text field.
    pub fn select_focused(&mut self) {
        if self.current_q >= self.questions.len() {
            return;
        }
        let n_opts = self.questions[self.current_q].options.len();
        if self.focused_option == n_opts {
            self.activate_other();
        } else {
            self.toggle_option(self.focused_option);
        }
    }

    /// Toggle selection of a regular option (for the current question).
    pub fn toggle_option(&mut self, index: usize) {
        if self.current_q >= self.questions.len() {
            return;
        }
        let q = &self.questions[self.current_q];
        if q.allow_multiple {
            if let Some(pos) = self.selected_options.iter().position(|&i| i == index) {
                self.selected_options.remove(pos);
            } else {
                self.selected_options.push(index);
                self.selected_options.sort_unstable();
            }
        } else {
            self.selected_options.clear();
            self.selected_options.push(index);
        }
        self.other_selected = false;
    }

    /// Activate the "Other" text field (moves focus to the Other row).
    pub fn activate_other(&mut self) {
        let n_opts = self.questions.get(self.current_q).map(|q| q.options.len()).unwrap_or(0);
        self.focused_option = n_opts;
        self.other_selected = true;
        self.selected_options.clear();
    }

    /// Toggle the "Other" option (keyboard shortcut 'O').
    pub fn toggle_other(&mut self) {
        if self.other_selected {
            self.other_selected = false;
        } else {
            self.activate_other();
        }
    }

    /// Deactivate the "Other" text field but keep the content.
    /// Returns focus to the "Other" row so the user can still see it.
    pub fn deactivate_other(&mut self) {
        self.other_selected = false;
    }

    /// Save the current question's state and advance to the next one.
    ///
    /// Returns `true` when all questions have been answered.
    pub fn submit(&mut self) -> bool {
        if self.current_q >= self.questions.len() {
            return true;
        }

        // Build the answer string.
        let q = &self.questions[self.current_q];
        let answer = if self.other_selected {
            let txt = self.other_input.trim();
            if txt.is_empty() { "Other".to_string() } else { format!("Other: {txt}") }
        } else if self.selected_options.is_empty() {
            "(no selection)".to_string()
        } else {
            self.selected_options
                .iter()
                .filter_map(|&i| q.options.get(i).cloned())
                .collect::<Vec<_>>()
                .join(", ")
        };
        self.answers.push(format!("Q: {}\nA: {}", q.prompt, answer));

        // Snapshot current state so the user can go back.
        self.snapshots.push(AnswerState {
            selected_options: self.selected_options.clone(),
            other_selected: self.other_selected,
            other_input: self.other_input.clone(),
            other_cursor: self.other_cursor,
            focused_option: self.focused_option,
        });

        self.current_q += 1;
        self.selected_options.clear();
        self.other_selected = false;
        self.other_input.clear();
        self.other_cursor = 0;
        self.focused_option = 0;

        self.current_q >= self.questions.len()
    }

    /// Navigate back to the previous question, restoring its saved state.
    ///
    /// Returns `false` if we are already on the first question.
    pub fn go_back(&mut self) -> bool {
        if self.current_q == 0 || self.snapshots.is_empty() {
            return false;
        }
        self.current_q -= 1;
        self.answers.pop();
        let snap = self.snapshots.pop().unwrap();
        self.selected_options = snap.selected_options;
        self.other_selected   = snap.other_selected;
        self.other_input      = snap.other_input;
        self.other_cursor     = snap.other_cursor;
        self.focused_option   = snap.focused_option;
        true
    }

    /// Send all collected answers back to the agent and consume `self`.
    pub fn finish(self) {
        let combined = self.answers.join("\n\n");
        let _ = self.answer_tx.send(combined);
    }

    /// Cancel and send a fallback message back to the agent.
    pub fn cancel(self) {
        let _ = self.answer_tx.send(
            "The user cancelled the question. Proceed with your best judgement.".into(),
        );
    }
}
