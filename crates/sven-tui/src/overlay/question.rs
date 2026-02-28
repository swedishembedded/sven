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
    /// Snapshot of `other_input` taken when text-edit mode is entered.
    /// Restored by Esc so the user can cancel an edit without losing previous text.
    pub other_input_snapshot: String,
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
            other_input_snapshot: String::new(),
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

    /// Activate the "Other" text field (enters text-edit mode).
    /// Saves the current `other_input` as a snapshot so Esc can restore it.
    pub fn activate_other(&mut self) {
        let n_opts = self
            .questions
            .get(self.current_q)
            .map(|q| q.options.len())
            .unwrap_or(0);
        self.focused_option = n_opts;
        self.other_selected = true;
        self.selected_options.clear();
        // Snapshot so Esc can restore without losing any previously accepted text.
        self.other_input_snapshot = self.other_input.clone();
    }

    /// Accept the typed text and exit text-edit mode.
    /// Focus stays on the "Other" row.
    pub fn deactivate_other(&mut self) {
        self.other_selected = false;
        // Accepted — cursor moves to end so the next edit starts there.
        self.other_cursor = self.other_input.len();
    }

    /// Cancel the current text edit: restore the snapshot and exit text-edit mode.
    pub fn cancel_other_edit(&mut self) {
        self.other_input = self.other_input_snapshot.clone();
        self.other_cursor = self.other_input.len();
        self.other_selected = false;
    }

    /// Returns `true` when the "Other" row has accepted (non-empty) text that
    /// was not yet submitted.
    pub fn other_has_text(&self) -> bool {
        !self.other_input.trim().is_empty()
    }

    /// Save the current question's state and advance to the next one.
    ///
    /// Returns `true` when all questions have been answered.
    pub fn submit(&mut self) -> bool {
        if self.current_q >= self.questions.len() {
            return true;
        }

        // Build the answer string.
        // Check other_input first: deactivate_other() clears other_selected while
        // keeping the text, so we must not rely on other_selected here.
        let q = &self.questions[self.current_q];
        let answer = if !self.other_input.trim().is_empty() {
            format!("Other: {}", self.other_input.trim())
        } else if self.other_selected {
            "Other".to_string()
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
        self.other_input_snapshot.clear();
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
        self.other_selected = snap.other_selected;
        self.other_input = snap.other_input;
        self.other_cursor = snap.other_cursor;
        self.focused_option = snap.focused_option;
        true
    }

    /// Send all collected answers back to the agent and consume `self`.
    pub fn finish(self) {
        let combined = self.answers.join("\n\n");
        let _ = self.answer_tx.send(combined);
    }

    /// Cancel and send a fallback message back to the agent.
    pub fn cancel(self) {
        let _ = self
            .answer_tx
            .send("The user cancelled the question. Proceed with your best judgement.".into());
    }
}
