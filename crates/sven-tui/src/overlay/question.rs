//! Question modal: multi-step question/answer flow triggered by the agent's
//! `AskQuestion` tool.

use sven_tools::Question;
use tokio::sync::oneshot;

/// Active multi-step question modal state.
pub struct QuestionModal {
    pub questions: Vec<Question>,
    /// Answers collected so far (one per completed question).
    pub answers: Vec<String>,
    pub current_q: usize,
    /// Selected option indices for current question (empty if using "Other")
    pub selected_options: Vec<usize>,
    /// True if "Other" is selected
    pub other_selected: bool,
    /// Text input for "Other" field
    pub other_input: String,
    pub other_cursor: usize,
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
            answer_tx,
        }
    }

    /// Toggle selection of an option (for current question).
    pub fn toggle_option(&mut self, index: usize) {
        if self.current_q >= self.questions.len() {
            return;
        }
        let q = &self.questions[self.current_q];
        
        if q.allow_multiple {
            // Multi-select: toggle
            if let Some(pos) = self.selected_options.iter().position(|&i| i == index) {
                self.selected_options.remove(pos);
            } else {
                self.selected_options.push(index);
                self.selected_options.sort_unstable();
            }
        } else {
            // Single-select: replace
            self.selected_options.clear();
            self.selected_options.push(index);
        }
        // Deselect "Other" when selecting a regular option
        self.other_selected = false;
    }

    /// Toggle the "Other" option.
    pub fn toggle_other(&mut self) {
        self.other_selected = !self.other_selected;
        if self.other_selected {
            // Clear regular selections when "Other" is selected
            self.selected_options.clear();
        }
    }

    /// Submit the current answer and move to the next question.
    ///
    /// Returns `true` when all questions have been answered and the modal
    /// should be closed (caller should then call `finish`).
    pub fn submit(&mut self) -> bool {
        if self.current_q >= self.questions.len() {
            return true;
        }

        let q = &self.questions[self.current_q];
        let answer = if self.other_selected {
            if self.other_input.trim().is_empty() {
                "Other".to_string()
            } else {
                format!("Other: {}", self.other_input.trim())
            }
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

        // Reset for next question
        self.selected_options.clear();
        self.other_selected = false;
        self.other_input.clear();
        self.other_cursor = 0;
        self.current_q += 1;

        self.current_q >= self.questions.len()
    }

    /// Build the final combined answer string and send it back to the agent.
    /// Consumes `self`.
    pub fn finish(self) {
        let combined = self.answers.join("\n\n");
        let _ = self.answer_tx.send(combined);
    }

    /// Cancel the modal by sending a fallback notice to the agent.
    pub fn cancel(self) {
        let _ = self.answer_tx.send(
            "The user cancelled the question. Proceed with your best judgement.".into(),
        );
    }
}
