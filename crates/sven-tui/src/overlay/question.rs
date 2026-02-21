//! Question modal: multi-step question/answer flow triggered by the agent's
//! `AskQuestion` tool.

use tokio::sync::oneshot;

/// Active multi-step question modal state.
pub struct QuestionModal {
    pub questions: Vec<String>,
    /// Answers collected so far (one per completed question).
    pub answers: Vec<String>,
    pub current_q: usize,
    pub input: String,
    pub cursor: usize,
    answer_tx: oneshot::Sender<String>,
}

impl QuestionModal {
    pub fn new(questions: Vec<String>, answer_tx: oneshot::Sender<String>) -> Self {
        Self {
            questions,
            answers: Vec::new(),
            current_q: 0,
            input: String::new(),
            cursor: 0,
            answer_tx,
        }
    }

    /// Submit the current input as the answer to the current question.
    ///
    /// Returns `true` when all questions have been answered and the modal
    /// should be closed (caller should then call `finish`).
    pub fn submit(&mut self) -> bool {
        let answer = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.answers.push(format!(
            "Q: {}\nA: {}",
            self.questions[self.current_q],
            answer,
        ));
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
