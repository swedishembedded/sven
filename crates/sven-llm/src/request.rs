//! Typed LLM request contracts.
//!
//! Each variant of [`LlmRequest`] is a named, well-scoped operation the HSM
//! machines issue to the reasoning service. The LLM **never** decides which
//! tool to run or what state to transition into; it only returns structured
//! data proposals that the HSM can inspect and act upon.
//!
//! Requests serialise to `serde_json::Value` so they can be carried inside
//! [`sven_hsm::Effect::CallLlm`] without the kernel knowing their shape.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A named, typed request to the LLM reasoning service.
///
/// Stored inside [`sven_hsm::Effect::CallLlm`] as a serialised
/// [`serde_json::Value`].  Each variant has a distinct output schema; the
/// [`crate::adapter::LlmAdapter`] knows how to convert that variant's response
/// JSON into the correct [`sven_hsm::Event`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LlmRequest {
    /// Classify the intent of a raw user utterance.
    ExtractIntent {
        /// Raw user text to classify.
        text: String,
        /// Only these intent labels may be returned.
        allowed_intents: Vec<String>,
    },

    /// Restate the user's intent as a structured problem statement.
    ExtractProblemStatement {
        /// Classified intent from a previous [`ExtractIntent`](LlmRequest::ExtractIntent) call.
        intent: String,
        /// Facts already known (serialised [`sven_hsm::Context`] subset).
        known_context: Value,
    },

    /// Extract hard constraints implied by the known context.
    ExtractConstraints {
        /// Facts already known.
        known_context: Value,
    },

    /// Decide whether the context has enough information to proceed.
    AssessCompleteness {
        /// Facts already known.
        known_context: Value,
        /// Field names that must be present and non-empty.
        required_fields: Vec<String>,
    },

    /// Generate the next clarifying question for the user.
    GenerateClarifyingQuestion {
        /// Names of information fields still missing.
        missing: Vec<String>,
        /// Facts already known.
        known_context: Value,
        /// Style / length policy (e.g. "keep it to one sentence").
        question_policy: String,
    },

    /// Generate a free-form conversational response to the user.
    ///
    /// Unlike the other variants this does not expect a JSON-shaped reply:
    /// the model's raw assistant text becomes
    /// [`sven_hsm::Event::LlmProposedResponse`].
    GenerateResponse {
        /// The interpreted intent / assessment from a prior
        /// [`ExtractIntent`](LlmRequest::ExtractIntent) call.
        intent: Value,
    },

    /// Parse a user's answer to a clarifying question into structured data.
    InterpretUserAnswer {
        /// The question that was asked.
        question: String,
        /// The raw user reply.
        answer: String,
        /// Description of the expected answer shape (used in the prompt).
        expected_answer_shape: String,
    },

    /// Generate one or more candidate high-level plans for the stated goal.
    GenerateCandidatePlan {
        /// Facts already known.
        known_context: Value,
        /// Planning style / risk tolerance policy.
        planning_policy: String,
    },

    /// Decompose a selected plan into ordered atomic work tasks.
    DecomposeIntoTasks {
        /// The chosen plan (from a previous [`GenerateCandidatePlan`](LlmRequest::GenerateCandidatePlan) response).
        selected_plan: Value,
        /// Constraints on task granularity / tooling.
        task_policy: String,
    },

    /// Propose a source-code patch for a single work task.
    ProposePatch {
        /// The work task to implement.
        task: Value,
        /// Relevant code context (file contents, diffs, …).
        code_context: Value,
        /// Patch-generation style / diff-format policy.
        patch_policy: String,
    },

    /// Parse and structure the raw output of a tool invocation.
    StructureToolObservation {
        /// The work task the tool was run for.
        task: Value,
        /// Raw stdout/stderr/exit-code from the tool.
        raw_output: String,
        /// Description of the expected structured observation shape.
        expected_observation: String,
    },

    /// Propose recovery options after a task or tool failure.
    ProposeRecoveryOptions {
        /// Description of the failure.
        failure: String,
        /// Facts already known.
        known_context: Value,
    },

    /// General-purpose context evaluation used by SDLC discovery, planning,
    /// execution, verification, and delivery states.
    ///
    /// The LLM examines `known_context` and returns a JSON assessment whose
    /// shape is described by `goal`. The HSM machine interprets the result
    /// and fires `LlmProposedAssessment` with whatever JSON the model returned.
    EvaluateContext {
        /// Human-readable description of what the LLM should evaluate /
        /// produce. This becomes the instruction in the system prompt.
        goal: String,
        /// Facts already known (serialised [`sven_hsm::Context`] subset).
        known_context: Value,
    },
}

impl LlmRequest {
    /// Returns `true` for variants whose LLM output is natural-language text
    /// meant to be shown directly to the user.
    ///
    /// Returning `false` means the response is a structured JSON payload that
    /// the machine parses internally; it must **not** be streamed as raw
    /// `TextDelta`/`TextComplete` events to the UI.
    #[must_use]
    pub fn is_user_facing(&self) -> bool {
        matches!(
            self,
            LlmRequest::GenerateResponse { .. } | LlmRequest::GenerateClarifyingQuestion { .. }
        )
    }

    /// Short human-readable name used in error messages and traces.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            LlmRequest::ExtractIntent { .. } => "ExtractIntent",
            LlmRequest::ExtractProblemStatement { .. } => "ExtractProblemStatement",
            LlmRequest::ExtractConstraints { .. } => "ExtractConstraints",
            LlmRequest::AssessCompleteness { .. } => "AssessCompleteness",
            LlmRequest::GenerateClarifyingQuestion { .. } => "GenerateClarifyingQuestion",
            LlmRequest::GenerateResponse { .. } => "GenerateResponse",
            LlmRequest::InterpretUserAnswer { .. } => "InterpretUserAnswer",
            LlmRequest::GenerateCandidatePlan { .. } => "GenerateCandidatePlan",
            LlmRequest::DecomposeIntoTasks { .. } => "DecomposeIntoTasks",
            LlmRequest::ProposePatch { .. } => "ProposePatch",
            LlmRequest::StructureToolObservation { .. } => "StructureToolObservation",
            LlmRequest::ProposeRecoveryOptions { .. } => "ProposeRecoveryOptions",
            LlmRequest::EvaluateContext { .. } => "EvaluateContext",
        }
    }

    /// Convert to a [`serde_json::Value`] for embedding inside
    /// [`sven_hsm::Effect::CallLlm`].
    ///
    /// # Panics
    ///
    /// Panics if serialisation fails, which cannot happen for this type.
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("LlmRequest serialisation is infallible")
    }

    /// Deserialise from an opaque [`serde_json::Value`] carried by
    /// [`sven_hsm::Effect::CallLlm`].
    ///
    /// # Errors
    ///
    /// Returns an error if the value is not a valid [`LlmRequest`].
    pub fn from_value(v: serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_through_json() {
        let req = LlmRequest::ExtractIntent {
            text: "fix the bug".into(),
            allowed_intents: vec!["bugfix".into(), "feature".into()],
        };
        let v = req.to_value();
        let back = LlmRequest::from_value(v).unwrap();
        assert_eq!(back.kind_name(), "ExtractIntent");
    }

    #[test]
    fn all_variants_have_distinct_kind_names() {
        let names: Vec<&'static str> = vec![
            LlmRequest::ExtractIntent {
                text: "t".into(),
                allowed_intents: vec![],
            }
            .kind_name(),
            LlmRequest::ExtractProblemStatement {
                intent: "i".into(),
                known_context: Value::Null,
            }
            .kind_name(),
            LlmRequest::ExtractConstraints {
                known_context: Value::Null,
            }
            .kind_name(),
            LlmRequest::AssessCompleteness {
                known_context: Value::Null,
                required_fields: vec![],
            }
            .kind_name(),
            LlmRequest::GenerateClarifyingQuestion {
                missing: vec![],
                known_context: Value::Null,
                question_policy: "p".into(),
            }
            .kind_name(),
            LlmRequest::GenerateResponse {
                intent: Value::Null,
            }
            .kind_name(),
            LlmRequest::InterpretUserAnswer {
                question: "q".into(),
                answer: "a".into(),
                expected_answer_shape: "s".into(),
            }
            .kind_name(),
            LlmRequest::GenerateCandidatePlan {
                known_context: Value::Null,
                planning_policy: "p".into(),
            }
            .kind_name(),
            LlmRequest::DecomposeIntoTasks {
                selected_plan: Value::Null,
                task_policy: "p".into(),
            }
            .kind_name(),
            LlmRequest::ProposePatch {
                task: Value::Null,
                code_context: Value::Null,
                patch_policy: "p".into(),
            }
            .kind_name(),
            LlmRequest::StructureToolObservation {
                task: Value::Null,
                raw_output: "o".into(),
                expected_observation: "e".into(),
            }
            .kind_name(),
            LlmRequest::ProposeRecoveryOptions {
                failure: "f".into(),
                known_context: Value::Null,
            }
            .kind_name(),
            LlmRequest::EvaluateContext {
                goal: "g".into(),
                known_context: Value::Null,
            }
            .kind_name(),
        ];
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "duplicate kind names");
    }

    #[test]
    fn is_user_facing_gates_correctly() {
        assert!(LlmRequest::GenerateResponse { intent: Value::Null }.is_user_facing());
        assert!(LlmRequest::GenerateClarifyingQuestion {
            missing: vec![],
            known_context: Value::Null,
            question_policy: "p".into(),
        }
        .is_user_facing());

        // Structured calls must NOT be user-facing.
        assert!(!LlmRequest::ExtractIntent {
            text: "t".into(),
            allowed_intents: vec![],
        }
        .is_user_facing());
        assert!(!LlmRequest::AssessCompleteness {
            known_context: Value::Null,
            required_fields: vec![],
        }
        .is_user_facing());
        assert!(!LlmRequest::EvaluateContext {
            goal: "g".into(),
            known_context: Value::Null,
        }
        .is_user_facing());
    }

    #[test]
    fn evaluate_context_round_trips_through_json() {
        let req = LlmRequest::EvaluateContext {
            goal: "classify the failure".into(),
            known_context: serde_json::json!({"failure": "tool error"}),
        };
        let v = req.to_value();
        assert_eq!(v["kind"], "evaluate_context");
        let back = LlmRequest::from_value(v).unwrap();
        assert_eq!(back.kind_name(), "EvaluateContext");
    }
}
