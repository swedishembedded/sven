//! Typed response types for each [`crate::request::LlmRequest`] variant.
//!
//! Each type here is the structured JSON the LLM is expected to return.
//! All types derive [`serde::Deserialize`] so they can be parsed from the
//! LLM's text output, and [`serde::Serialize`] so they can be re-encoded
//! into the [`serde_json::Value`] payload of the appropriate
//! [`sven_hsm::Event`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── ExtractIntent ─────────────────────────────────────────────────────────────

/// Response to [`LlmRequest::ExtractIntent`](crate::request::LlmRequest::ExtractIntent).
///
/// Carried inside `Event::LlmProposedAssessment { assessment }`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IntentExtraction {
    /// The classified intent label (one of the `allowed_intents`).
    pub intent: String,
    /// Model's confidence: 0.0 = none, 1.0 = certain.
    pub confidence: f32,
}

// ── ExtractProblemStatement ───────────────────────────────────────────────────

/// Response to [`LlmRequest::ExtractProblemStatement`](crate::request::LlmRequest::ExtractProblemStatement).
///
/// Carried inside `Event::LlmProposedAssessment { assessment }`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProblemStatement {
    /// Concise problem statement in one or two sentences.
    pub statement: String,
    /// Domain keywords extracted from the statement.
    pub keywords: Vec<String>,
}

// ── ExtractConstraints ────────────────────────────────────────────────────────

/// Response to [`LlmRequest::ExtractConstraints`](crate::request::LlmRequest::ExtractConstraints).
///
/// Carried inside `Event::LlmProposedAssessment { assessment }`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Constraints {
    /// Each element is a single constraint sentence.
    pub items: Vec<String>,
}

// ── AssessCompleteness ────────────────────────────────────────────────────────

/// Describes one piece of information that is missing or unclear.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissingInfo {
    /// Name of the missing field (matching one of `required_fields`).
    pub field: String,
    /// Why it is required and what the user should provide.
    pub reason: String,
}

/// Status discriminant in [`CompletenessAssessment`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CompletenessStatus {
    /// All required fields are present; the machine may proceed.
    Enough,
    /// One or more required fields are missing.
    Missing {
        /// The fields that need to be filled in.
        fields: Vec<MissingInfo>,
    },
    /// Proceeding is impossible due to an unresolvable conflict or constraint.
    Blocked {
        /// Human-readable explanation of why the machine is blocked.
        reason: String,
    },
}

/// Response to [`LlmRequest::AssessCompleteness`](crate::request::LlmRequest::AssessCompleteness).
///
/// Carried inside `Event::LlmProposedAssessment { assessment }`.
///
/// Uses `#[serde(flatten)]` so the JSON shape matches what the LLM returns:
/// `{"status":"enough"}` or `{"status":"missing","fields":[...]}`.
/// Without flatten, the `status` field name collides with the internally-tagged
/// enum's own `"status"` discriminant key, producing the wrong nested shape
/// `{"status":{"status":"enough"}}` which the LLM never produces.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletenessAssessment {
    /// Whether the context is sufficient to proceed.
    #[serde(flatten)]
    pub status: CompletenessStatus,
}

// ── GenerateClarifyingQuestion ────────────────────────────────────────────────

/// Response to [`LlmRequest::GenerateClarifyingQuestion`](crate::request::LlmRequest::GenerateClarifyingQuestion).
///
/// Carried inside `Event::LlmProposedResponse { text }` (the question text).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClarifyingQuestion {
    /// The question to ask the user.
    pub question: String,
    /// Optional hint to help the user answer.
    pub hint: Option<String>,
}

// ── InterpretUserAnswer ───────────────────────────────────────────────────────

/// Response to [`LlmRequest::InterpretUserAnswer`](crate::request::LlmRequest::InterpretUserAnswer).
///
/// Carried inside `Event::LlmProposedAssessment { assessment }`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnswerInterpretation {
    /// Structured data extracted from the user's free-form answer.
    pub extracted: Value,
    /// How confident the model is in the extraction (0.0–1.0).
    pub confidence: f32,
}

// ── GenerateCandidatePlan ─────────────────────────────────────────────────────

/// Response to [`LlmRequest::GenerateCandidatePlan`](crate::request::LlmRequest::GenerateCandidatePlan).
///
/// Carried inside `Event::LlmProposedPlan { plan }`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidatePlan {
    /// Short title for this plan option.
    pub title: String,
    /// Ordered list of high-level steps.
    pub steps: Vec<String>,
    /// Risk level: `"low"`, `"medium"`, or `"high"`.
    pub risk: String,
}

// ── DecomposeIntoTasks ────────────────────────────────────────────────────────

/// A single atomic work task produced by decomposition.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkTask {
    /// Stable identifier for this task (e.g. `"t1"`, `"t2"`).
    pub id: String,
    /// Human-readable description of the work to do.
    pub description: String,
    /// Optional tool hint (tool name to use, if applicable).
    pub tool: Option<String>,
    /// Tool arguments (may be `Value::Null` if no tool is hinted).
    pub args: Value,
}

/// Response to [`LlmRequest::DecomposeIntoTasks`](crate::request::LlmRequest::DecomposeIntoTasks).
///
/// Carried inside `Event::LlmProposedPlan { plan }`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskDecomposition {
    /// Ordered list of atomic tasks.
    pub tasks: Vec<WorkTask>,
}

// ── ProposePatch ──────────────────────────────────────────────────────────────

/// Response to [`LlmRequest::ProposePatch`](crate::request::LlmRequest::ProposePatch).
///
/// Carried inside `Event::LlmProposedPlan { plan }`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PatchProposal {
    /// The unified diff to apply.
    pub diff: String,
    /// Plain-language explanation of what the patch does and why.
    pub explanation: String,
}

// ── StructureToolObservation ──────────────────────────────────────────────────

/// Response to [`LlmRequest::StructureToolObservation`](crate::request::LlmRequest::StructureToolObservation).
///
/// Carried inside `Event::LlmProposedAssessment { assessment }`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolObservation {
    /// One-sentence human-readable summary.
    pub summary: String,
    /// Structured data extracted from the raw output.
    pub structured: Value,
}

// ── ProposeRecoveryOptions ────────────────────────────────────────────────────

/// A single recovery action option.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryOption {
    /// Short label for this option (e.g. `"retry"`, `"rollback"`, `"skip"`).
    pub label: String,
    /// Plain-language description of what this option does.
    pub description: String,
}

/// Response to [`LlmRequest::ProposeRecoveryOptions`](crate::request::LlmRequest::ProposeRecoveryOptions).
///
/// Carried inside `Event::LlmProposedAssessment { assessment }`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryOptions {
    /// Ordered list of possible recovery actions.
    pub options: Vec<RecoveryOption>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completeness_status_round_trips() {
        let s = CompletenessStatus::Missing {
            fields: vec![MissingInfo {
                field: "repo_url".into(),
                reason: "needed to clone".into(),
            }],
        };
        let a = CompletenessAssessment { status: s };
        let json = serde_json::to_string(&a).unwrap();
        let back: CompletenessAssessment = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.status, CompletenessStatus::Missing { .. }));
    }

    #[test]
    fn completeness_enough_round_trips() {
        let a = CompletenessAssessment {
            status: CompletenessStatus::Enough,
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: CompletenessAssessment = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.status, CompletenessStatus::Enough));
    }

    #[test]
    fn clarifying_question_without_hint() {
        let q = ClarifyingQuestion {
            question: "What is the target platform?".into(),
            hint: None,
        };
        let json = serde_json::to_string(&q).unwrap();
        assert!(!json.contains("\"hint\"") || json.contains("null"));
        let back: ClarifyingQuestion = serde_json::from_str(&json).unwrap();
        assert_eq!(back.question, "What is the target platform?");
    }
}
