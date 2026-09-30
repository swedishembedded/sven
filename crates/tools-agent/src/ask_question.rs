// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use sven_hsm::ToolCapability;

use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};
use sven_vocab::provenance::FactSource;

/// A single structured question with multiple-choice options.
#[derive(Debug, Clone)]
pub struct Question {
    pub prompt: String,
    pub options: Vec<String>,
    pub allow_multiple: bool,
}

/// Sent to the TUI when the agent asks a question; the TUI sends the answer
/// back via `answer_tx`.
pub struct QuestionRequest {
    pub id: String,
    pub questions: Vec<Question>,
    pub answer_tx: oneshot::Sender<String>,
}

/// Interactively ask the user one or more questions and collect their answers.
///
/// In TUI mode a `question_tx` channel is provided; the tool sends a
/// [`QuestionRequest`] and awaits the answer from the UI.  In plain terminal
/// mode stdin must be a TTY; in headless/CI mode the tool parks instead of
/// blocking or guessing - see [`sven_vocab::ParkedAnswer`].
pub struct AskQuestionTool {
    /// When set, routes questions to the TUI instead of reading from stdin.
    question_tx: Option<mpsc::Sender<QuestionRequest>>,
    /// Force headless mode regardless of TTY detection. Used in tests and CI.
    force_headless: bool,
}

impl AskQuestionTool {
    pub fn new() -> Self {
        Self {
            question_tx: None,
            force_headless: false,
        }
    }

    /// Create a TUI-aware instance that sends questions via `tx`.
    pub fn new_tui(tx: mpsc::Sender<QuestionRequest>) -> Self {
        Self {
            question_tx: Some(tx),
            force_headless: false,
        }
    }

    /// Create an instance that always behaves as headless (non-interactive).
    /// Use in tests and CI environments where stdin must not be read.
    pub fn new_headless() -> Self {
        Self {
            question_tx: None,
            force_headless: true,
        }
    }
}

impl Default for AskQuestionTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for AskQuestionTool {
    fn name(&self) -> &str {
        "ask_question"
    }

    fn description(&self) -> &str {
        "Present structured multiple-choice questions to the user and collect responses.\n\
         Each question: prompt, options (≥2). allow_multiple: false by default.\n\
         Do NOT include 'Other' in options - it is always appended automatically.\n\
         In headless/CI/piped mode this parks rather than answering immediately:\n\
         the run pauses until a human answers out of band, however long that takes.\n\
         Use for decisions requiring explicit choice; for yes/no just ask directly in text."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "prompt": {
                                "type": "string",
                                "description": "The question to ask"
                            },
                            "options": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "List of choices. Do NOT add 'Other' - it is appended automatically.",
                                "minItems": 2
                            },
                            "allow_multiple": {
                                "type": "boolean",
                                "description": "Whether multiple options can be selected (default: false)",
                                "default": false
                            }
                        },
                        "required": ["prompt", "options"],
                        "additionalProperties": false
                    },
                    "description": "List of 1-3 questions",
                    "minItems": 1,
                    "maxItems": 3
                }
            },
            "required": ["questions"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let questions_json = match call.args.get("questions").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => return ToolOutput::err(&call.id, "missing 'questions' array"),
        };

        let mut questions: Vec<Question> = Vec::new();
        for (i, q_val) in questions_json.iter().enumerate() {
            let q_obj = match q_val.as_object() {
                Some(o) => o,
                None => {
                    return ToolOutput::err(
                        &call.id,
                        format!("question {} is not an object", i + 1),
                    )
                }
            };

            let prompt = match q_obj.get("prompt").and_then(|v| v.as_str()) {
                Some(p) => p.to_string(),
                None => {
                    return ToolOutput::err(
                        &call.id,
                        format!("question {} missing 'prompt'", i + 1),
                    )
                }
            };

            let options: Vec<String> = match q_obj.get("options").and_then(|v| v.as_array()) {
                Some(opts) => opts
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                None => {
                    return ToolOutput::err(
                        &call.id,
                        format!("question {} missing 'options'", i + 1),
                    )
                }
            };

            if options.len() < 2 {
                return ToolOutput::err(
                    &call.id,
                    format!("question {} needs at least 2 options", i + 1),
                );
            }

            let allow_multiple = q_obj
                .get("allow_multiple")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            questions.push(Question {
                prompt,
                options,
                allow_multiple,
            });
        }

        if questions.is_empty() {
            return ToolOutput::err(&call.id, "questions array must not be empty");
        }
        if questions.len() > 3 {
            return ToolOutput::err(&call.id, "at most 3 questions may be asked at a time");
        }

        debug!(count = questions.len(), "ask_question tool");

        // ── TUI mode ─────────────────────────────────────────────────────────
        if let Some(tx) = &self.question_tx {
            let (answer_tx, answer_rx) = oneshot::channel();
            let questions_for_provenance = questions.clone();
            let req = QuestionRequest {
                id: call.id.clone(),
                questions,
                answer_tx,
            };
            if tx.send(req).await.is_err() {
                return ToolOutput::err(&call.id, "TUI question channel closed unexpectedly");
            }
            return match answer_rx.await {
                // The user answered. This tool writes nothing itself - it
                // only attaches the provenance of the answer, as either `UserChoice` (a genuine pick between named
                // alternatives) or `UserStated` (the user's own words).
                Ok(answer) => {
                    let source = choice_or_stated(&call.id, &questions_for_provenance, &answer);
                    ToolOutput::ok(&call.id, answer).with_provenance(source)
                }
                Err(_) => ToolOutput::err(&call.id, "Question was cancelled by the user"),
            };
        }

        // ── Plain terminal / headless mode ────────────────────────────────────
        // Cannot be answered synchronously - and must not be guessed at either
        // (a fabricated "proceed with your best judgement" answer is exactly
        // the self-grading failure mode this tool must not enable). Park it:
        // the run stops advancing on this call, `Event::QuestionAsked` records
        // why, and it resumes only from a real `Event::HumanAnswered`, however
        // long that takes. See `sven_hsm::event::Event::QuestionAsked`.
        if self.force_headless || !stdin_is_tty() {
            // The parking primitive carries one prompt/one option set (see
            // `Effect::RequestHumanAnswer`) - the same reasoning
            // `choice_or_stated` already applies to provenance: only a single
            // question can honestly carry its own options through. Multiple
            // questions are combined into one free-form prompt instead of
            // silently answering only the first.
            let (prompt, options) = match questions.as_slice() {
                [only] => (only.prompt.clone(), only.options.clone()),
                many => {
                    let combined = many
                        .iter()
                        .enumerate()
                        .map(|(i, q)| format!("{}. {}", i + 1, q.prompt))
                        .collect::<Vec<_>>()
                        .join("\n");
                    (combined, Vec::new())
                }
            };
            return ToolOutput::parked(&call.id, prompt, options);
        }

        eprintln!();
        eprintln!("╔══ Questions from agent ══════════════════════════╗");
        for (i, q) in questions.iter().enumerate() {
            eprintln!("  {}. {}", i + 1, q.prompt);
            for (j, opt) in q.options.iter().enumerate() {
                eprintln!("     {}. {}", j + 1, opt);
            }
            eprintln!("     {}. Other (type your answer)", q.options.len() + 1);
            if q.allow_multiple {
                eprintln!("     (You can select multiple: e.g. \"1,2\" or \"3\")");
            }
        }
        eprintln!("╚══════════════════════════════════════════════════╝");

        let mut answers: Vec<String> = Vec::new();
        let mut raw_answers: Vec<String> = Vec::new();
        for (i, q) in questions.iter().enumerate() {
            eprint!("  Answer {}: ", i + 1);
            let input = read_stdin_line().await;
            let answer = parse_stdin_answer(&input, &q.options, q.allow_multiple);
            answers.push(format!("Q: {}\nA: {}", q.prompt, answer));
            raw_answers.push(answer);
        }
        eprintln!();

        // Only the single-question case can name a genuine choice; see
        // `choice_or_stated` for why that is the only shape `FactSource::
        // UserChoice` can honestly represent.
        let source = raw_answers
            .first()
            .map(|answer| choice_or_stated(&call.id, &questions, answer))
            .unwrap_or(FactSource::UserStated);
        ToolOutput::ok(&call.id, answers.join("\n\n")).with_provenance(source)
    }
}

/// Decides whether an answer names a genuine choice between offered
/// alternatives ([`FactSource::UserChoice`]) or is just the user's own words
/// ([`FactSource::UserStated`]).
///
/// Only a single question whose answer names exactly one of its own options
/// counts as a choice: a `ToolOutput` carries at most one [`FactSource`], so a
/// multi-question exchange, or a free-form "Other" answer, cannot be honestly
/// split into a chosen/not_chosen pair and is recorded as the user's own
/// statement instead. Capturing what was picked *and* what was rejected is
/// the entire point of this variant - a record holding only the chosen answer
/// loses what the user turned down.
fn choice_or_stated(question_id: &str, questions: &[Question], answer: &str) -> FactSource {
    match questions {
        [only] if only.options.iter().any(|opt| opt == answer) => FactSource::UserChoice {
            question_id: question_id.to_string(),
            chosen: answer.to_string(),
            not_chosen: only
                .options
                .iter()
                .filter(|opt| opt.as_str() != answer)
                .cloned()
                .collect(),
        },
        _ => FactSource::UserStated,
    }
}

/// Returns true only when stdin is connected to an interactive terminal.
/// Uses `libc::isatty` on Unix; always false on other platforms.
fn stdin_is_tty() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: isatty is async-signal-safe and only reads an fd number.
        unsafe { libc::isatty(std::io::stdin().as_raw_fd()) != 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

async fn read_stdin_line() -> String {
    use tokio::io::AsyncBufReadExt;
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(_) => line
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string(),
        Err(_) => String::new(),
    }
}

/// Parse stdin input for multiple-choice questions.
/// Input can be:
/// - "1" for option 1
/// - "1,2,3" for multiple options
/// - "other: custom text" for custom answer
fn parse_stdin_answer(input: &str, options: &[String], allow_multiple: bool) -> String {
    let input = input.trim();

    // Check for "other:" prefix (case-insensitive)
    if input.to_lowercase().starts_with("other:") || input.to_lowercase().starts_with("other ") {
        let text = input[6..].trim();
        if text.is_empty() {
            return "Other (no text provided)".to_string();
        }
        return format!("Other: {}", text);
    }

    // Try to parse as comma-separated numbers
    let selections: Vec<usize> = input
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n > 0 && n <= options.len() + 1)
        .collect();

    if selections.is_empty() {
        // If parsing failed, treat as "Other" with custom text
        return if input.is_empty() {
            "(no selection made)".to_string()
        } else {
            format!("Other: {}", input)
        };
    }

    // Check if "Other" option (last number) was selected
    let other_idx = options.len() + 1;
    if selections.contains(&other_idx) {
        return "Other".to_string();
    }

    // Map selections to option strings
    let selected: Vec<String> = selections
        .iter()
        .filter_map(|&n| options.get(n - 1).cloned())
        .collect();

    if selected.is_empty() {
        return "(no valid selection)".to_string();
    }

    if !allow_multiple && selected.len() > 1 {
        // If multiple not allowed, take first
        selected[0].clone()
    } else {
        selected.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_tool_api::tool::Tool;

    #[test]
    fn schema_requires_questions() {
        let t = AskQuestionTool::new();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("questions")));
    }

    #[tokio::test]
    async fn missing_questions_is_error() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;
        let t = AskQuestionTool::new();
        let call = ToolCall {
            id: "1".into(),
            name: "ask_question".into(),
            args: json!({}),
        };
        let out = t.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'questions'"));
    }

    #[tokio::test]
    async fn too_many_questions_is_error() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;
        let t = AskQuestionTool::new();
        let make_q = |prompt: &str| {
            json!({
                "prompt": prompt,
                "options": ["Yes", "No"],
            })
        };
        let call = ToolCall {
            id: "1".into(),
            name: "ask_question".into(),
            args: json!({
                "questions": [make_q("q1"), make_q("q2"), make_q("q3"), make_q("q4")]
            }),
        };
        let out = t.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("at most 3"));
    }

    /// In headless/CI mode the tool must park rather than block forever
    /// waiting for interactive input - and, just as importantly, rather than
    /// fabricate an answer. A single question keeps its own options through
    /// the park.
    #[tokio::test]
    async fn headless_mode_parks_a_single_question_with_its_options() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;

        // Use new_headless() so the test is deterministic regardless of whether
        // the test runner inherits a TTY from the calling terminal.
        let t = AskQuestionTool::new_headless();
        let call = ToolCall {
            id: "1".into(),
            name: "ask_question".into(),
            args: json!({
                "questions": [
                    { "prompt": "What language?", "options": ["Rust", "Python", "Go"] },
                ]
            }),
        };
        let out = t.execute(&call).await;
        assert!(
            !out.is_error,
            "a parked call has not failed - it has not concluded"
        );
        let parked = out.parked.expect("headless mode must park, not guess");
        assert_eq!(parked.prompt, "What language?");
        assert_eq!(
            parked.options,
            vec!["Rust".to_string(), "Python".to_string(), "Go".to_string()]
        );
        assert!(
            out.provenance.is_none(),
            "no answer was ever given, so there is nothing to attribute to the user"
        );
    }

    /// Multiple questions cannot each keep their own options through a single
    /// parked call (the primitive carries one prompt/one option set) - they
    /// are combined into one free-form prompt instead of silently answering
    /// only the first and discarding the rest.
    #[tokio::test]
    async fn headless_mode_parks_multiple_questions_as_one_combined_free_form_prompt() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;

        let t = AskQuestionTool::new_headless();
        let call = ToolCall {
            id: "1".into(),
            name: "ask_question".into(),
            args: json!({
                "questions": [
                    { "prompt": "What language?", "options": ["Rust", "Python", "Go"] },
                    { "prompt": "What framework?", "options": ["Axum", "Actix", "Rocket"] },
                ]
            }),
        };
        let out = t.execute(&call).await;
        let parked = out.parked.expect("headless mode must park, not guess");
        assert!(parked.prompt.contains("What language?"));
        assert!(parked.prompt.contains("What framework?"));
        assert!(
            parked.options.is_empty(),
            "a combined multi-question prompt cannot honestly carry either question's own options"
        );
    }

    /// A free-form answer - one that does not name any of the offered
    /// options, e.g. "Other" text the user typed themselves - is
    /// `FactSource::UserStated`: the user's own words, this session, but not
    /// a choice between named alternatives. This tool never writes memory
    /// itself; it only attaches the provenance of the answer.
    #[tokio::test]
    async fn a_free_form_tui_answer_attaches_user_stated_provenance() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;

        let (tx, mut rx) = mpsc::channel(1);
        let t = AskQuestionTool::new_tui(tx);
        let call = ToolCall {
            id: "1".into(),
            name: "ask_question".into(),
            args: json!({
                "questions": [
                    { "prompt": "Which framework?", "options": ["Axum", "Actix"] },
                ]
            }),
        };

        let execute = tokio::spawn(async move { t.execute(&call).await });
        let req = rx.recv().await.expect("question request sent");
        req.answer_tx
            .send("Other: gRPC".to_string())
            .expect("answer channel open");

        let out = execute.await.expect("execute task joins");
        assert!(!out.is_error);
        assert_eq!(out.provenance, Some(Box::new(FactSource::UserStated)));
    }

    /// A genuine pick between offered alternatives - the answer names exactly
    /// one of a single question's options - is `FactSource::UserChoice`,
    /// recording both what was chosen and what was rejected. An uncaptured
    /// choice is gone forever the moment the session ends; a later DPO
    /// objective needs both sides of the pair, not just the winner.
    #[tokio::test]
    async fn a_user_choice_records_both_the_chosen_and_the_not_chosen_options() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;

        let (tx, mut rx) = mpsc::channel(1);
        let t = AskQuestionTool::new_tui(tx);
        let call = ToolCall {
            id: "call-42".into(),
            name: "ask_question".into(),
            args: json!({
                "questions": [
                    { "prompt": "Which framework?", "options": ["Axum", "Actix", "Rocket"] },
                ]
            }),
        };

        let execute = tokio::spawn(async move { t.execute(&call).await });
        let req = rx.recv().await.expect("question request sent");
        req.answer_tx
            .send("Axum".to_string())
            .expect("answer channel open");

        let out = execute.await.expect("execute task joins");
        assert!(!out.is_error);
        match out.provenance.map(|b| *b) {
            Some(FactSource::UserChoice {
                question_id,
                chosen,
                not_chosen,
            }) => {
                assert_eq!(question_id, "call-42");
                assert_eq!(chosen, "Axum");
                assert_eq!(not_chosen, vec!["Actix".to_string(), "Rocket".to_string()]);
            }
            other => panic!("expected UserChoice provenance, got {other:?}"),
        }
    }
}
