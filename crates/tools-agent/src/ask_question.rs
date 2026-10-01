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
use sven_vocab::NO_USER_ANSWER;

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

/// Ask the user one or more questions and collect their answers.
///
/// Where the questions go is fixed when the tool is built, never guessed from
/// the process's terminal: to a surface a person answers while the run waits
/// ([`Self::new_tui`]), parked for an answer that may come much later
/// ([`Self::parking`]), or - with nobody to ask - answered at once with
/// [`NO_USER_ANSWER`] ([`Self::no_user`]).
pub struct AskQuestionTool {
    asking: Asking,
}

/// Who answers the questions.
enum Asking {
    /// A person, through the surface holding the other end.
    Person(mpsc::Sender<QuestionRequest>),
    /// Nobody now: the run parks on the question - see
    /// [`sven_vocab::ParkedAnswer`].
    Parked,
    /// Nobody at all: the question is answered with [`NO_USER_ANSWER`].
    NoUser,
}

impl AskQuestionTool {
    /// Sends each question to `tx`, whose holder shows it to a person and
    /// replies with the answer.
    pub fn new_tui(tx: mpsc::Sender<QuestionRequest>) -> Self {
        Self {
            asking: Asking::Person(tx),
        }
    }

    /// Parks the run on each question until an answer is posted for it.
    pub fn parking() -> Self {
        Self {
            asking: Asking::Parked,
        }
    }

    /// Answers each question at once with [`NO_USER_ANSWER`]: for a session
    /// nobody is at.
    pub fn no_user() -> Self {
        Self {
            asking: Asking::NoUser,
        }
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
         When no user is available it says so at once; then decide yourself and\n\
         state the assumption you made.\n\
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

        let tx = match &self.asking {
            Asking::Person(tx) => tx,
            // Parking cannot be answered synchronously - and must not be
            // guessed at either. The run stops advancing on this call,
            // `Event::QuestionAsked` records why, and it resumes only from a
            // real `Event::HumanAnswered`. The parking primitive carries one
            // prompt/one option set (see `Effect::RequestHumanAnswer`), so
            // several questions are combined into one free-form prompt
            // instead of silently answering only the first.
            Asking::Parked => {
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
            // Nobody will ever answer. Saying so is not an answer on the
            // user's behalf, so it carries no user provenance.
            Asking::NoUser => return ToolOutput::ok(&call.id, NO_USER_ANSWER),
        };

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
        match answer_rx.await {
            // The user answered. This tool writes nothing itself - it
            // only attaches the provenance of the answer, as either `UserChoice` (a genuine pick between named
            // alternatives) or `UserStated` (the user's own words).
            Ok(answer) => {
                let source = choice_or_stated(&call.id, &questions_for_provenance, &answer);
                ToolOutput::ok(&call.id, answer).with_provenance(source)
            }
            Err(_) => ToolOutput::err(&call.id, "Question was cancelled by the user"),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use sven_tool_api::tool::Tool;

    #[test]
    fn schema_requires_questions() {
        let t = AskQuestionTool::no_user();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("questions")));
    }

    #[tokio::test]
    async fn missing_questions_is_error() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;
        let t = AskQuestionTool::no_user();
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
        let t = AskQuestionTool::no_user();
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

    /// Nobody at the session: the question is answered at once, saying so,
    /// and nothing is attributed to the user.
    #[tokio::test]
    async fn with_no_user_a_question_is_answered_at_once_saying_so() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;

        let t = AskQuestionTool::no_user();
        let call = ToolCall {
            id: "1".into(),
            name: "ask_question".into(),
            args: json!({
                "questions": [{ "prompt": "What language?", "options": ["Rust", "Go"] }]
            }),
        };
        let out = tokio::time::timeout(std::time::Duration::from_secs(5), t.execute(&call))
            .await
            .expect("answered without waiting for anyone");
        assert!(!out.is_error);
        assert!(out.parked.is_none());
        assert_eq!(out.content, NO_USER_ANSWER);
        assert!(out.provenance.is_none());
    }

    /// A parking tool parks rather than fabricating an answer. A single
    /// question keeps its own options through the park.
    #[tokio::test]
    async fn parking_parks_a_single_question_with_its_options() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;

        let t = AskQuestionTool::parking();
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
        let parked = out.parked.expect("a parking tool parks, it does not guess");
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
    async fn parking_combines_multiple_questions_into_one_free_form_prompt() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;

        let t = AskQuestionTool::parking();
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
        let parked = out.parked.expect("a parking tool parks, it does not guess");
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
