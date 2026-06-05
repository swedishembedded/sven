//! `sven-llm` — typed LLM operation contracts for the Sven HSM runtime.
//!
//! # Purpose
//!
//! This crate wraps `sven-model` (the stateless provider abstraction) behind
//! **named, typed LLM operations** so that the HSM machines never issue raw
//! prompts and never need to know which model is being used.
//!
//! # Architecture
//!
//! ```text
//! Effect::CallLlm { request: Value }
//!          │
//!          ▼  (sven-executors::LlmExecutor)
//!  LlmRequest  ──► LlmAdapter::invoke
//!          │
//!          ├─► build_prompt()  →  CompletionRequest  →  ModelProvider::complete()
//!          │                      (stream of ResponseEvent)
//!          │
//!          └─► accumulate + parse JSON  →  sven_hsm::Event
//! ```
//!
//! The LLM **never** names a tool to run or a state to transition into; it
//! only returns structured data proposals in JSON, which the HSM inspects and
//! acts upon deterministically.
//!
//! # Testing
//!
//! Use [`mock::MockLlmAdapter`] in unit tests.  It accepts a pre-programmed
//! `Vec<Event>` and dequeues one event per `invoke` call — no network access
//! required.

pub mod adapter;
pub mod error;
pub mod mock;
pub mod request;
pub mod response;

// Re-export the most important types at the crate root.
pub use adapter::{DefaultLlmAdapter, LlmAdapter};
pub use error::LlmError;
pub use mock::MockLlmAdapter;
pub use request::LlmRequest;

// ── Per-variant integration tests using MockLlmAdapter ────────────────────────
//
// Each test constructs the corresponding LlmRequest, pre-programs a mock
// adapter with the expected Event, calls invoke, and asserts the returned
// event has the correct kind.

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sven_hsm::{Event, EventKind};

    use crate::adapter::LlmAdapter;
    use crate::mock::MockLlmAdapter;
    use crate::request::LlmRequest;

    fn assessment(payload: serde_json::Value) -> Event {
        Event::LlmProposedAssessment {
            assessment: payload,
        }
    }

    fn plan(payload: serde_json::Value) -> Event {
        Event::LlmProposedPlan { plan: payload }
    }

    fn response_text(text: &str) -> Event {
        Event::LlmProposedResponse { text: text.into() }
    }

    // ── ExtractIntent ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn extract_intent_returns_assessment() {
        let expected = assessment(json!({"intent": "bugfix", "confidence": 0.9}));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::ExtractIntent {
            text: "fix the crash in auth".into(),
            allowed_intents: vec!["bugfix".into(), "feature".into()],
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedAssessment);
        assert_eq!(event, expected);
    }

    // ── ExtractProblemStatement ───────────────────────────────────────────────

    #[tokio::test]
    async fn extract_problem_statement_returns_assessment() {
        let expected = assessment(json!({
            "statement": "Fix null pointer dereference in auth module.",
            "keywords": ["auth", "null pointer", "crash"]
        }));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::ExtractProblemStatement {
            intent: "bugfix".into(),
            known_context: json!({"description": "auth crashes on nil user"}),
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedAssessment);
    }

    // ── ExtractConstraints ────────────────────────────────────────────────────

    #[tokio::test]
    async fn extract_constraints_returns_assessment() {
        let expected = assessment(json!({"items": ["must not break existing tests"]}));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::ExtractConstraints {
            known_context: json!({"goal": "refactor auth"}),
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedAssessment);
    }

    // ── AssessCompleteness ────────────────────────────────────────────────────

    #[tokio::test]
    async fn assess_completeness_enough_returns_assessment() {
        let expected = assessment(json!({"status": "enough"}));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::AssessCompleteness {
            known_context: json!({"goal": "done", "repo": "https://github.com/x/y"}),
            required_fields: vec!["goal".into(), "repo".into()],
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedAssessment);
    }

    #[tokio::test]
    async fn assess_completeness_missing_returns_assessment() {
        let expected = assessment(json!({
            "status": "missing",
            "fields": [{"field": "repo", "reason": "needed to clone"}]
        }));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::AssessCompleteness {
            known_context: json!({"goal": "fix bug"}),
            required_fields: vec!["goal".into(), "repo".into()],
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedAssessment);
    }

    // ── GenerateClarifyingQuestion ────────────────────────────────────────────

    #[tokio::test]
    async fn generate_clarifying_question_returns_proposed_response() {
        let expected = response_text("What is the repository URL?");
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::GenerateClarifyingQuestion {
            missing: vec!["repo_url".into()],
            known_context: json!({}),
            question_policy: "one sentence".into(),
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedResponse);
        assert_eq!(event, expected);
    }

    // ── InterpretUserAnswer ───────────────────────────────────────────────────

    #[tokio::test]
    async fn interpret_user_answer_returns_assessment() {
        let expected = assessment(json!({
            "extracted": {"repo_url": "https://github.com/x/y"},
            "confidence": 0.95
        }));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::InterpretUserAnswer {
            question: "What is the repository URL?".into(),
            answer: "It's at https://github.com/x/y".into(),
            expected_answer_shape: "{\"repo_url\": \"string\"}".into(),
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedAssessment);
    }

    // ── GenerateCandidatePlan ─────────────────────────────────────────────────

    #[tokio::test]
    async fn generate_candidate_plan_returns_proposed_plan() {
        let expected = plan(json!({
            "title": "Minimal fix",
            "steps": ["read file", "fix bug", "run tests"],
            "risk": "low"
        }));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::GenerateCandidatePlan {
            known_context: json!({"goal": "fix crash", "repo": "https://github.com/x/y"}),
            planning_policy: "conservative".into(),
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedPlan);
    }

    // ── DecomposeIntoTasks ────────────────────────────────────────────────────

    #[tokio::test]
    async fn decompose_into_tasks_returns_proposed_plan() {
        let expected = plan(json!({
            "tasks": [
                {"id": "t1", "description": "read auth.rs", "tool": "read_file", "args": {"path": "src/auth.rs"}},
                {"id": "t2", "description": "apply fix", "tool": null, "args": {}}
            ]
        }));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::DecomposeIntoTasks {
            selected_plan: json!({"title": "Minimal fix", "steps": ["read", "fix"]}),
            task_policy: "atomic tasks".into(),
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedPlan);
    }

    // ── ProposePatch ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn propose_patch_returns_proposed_plan() {
        let expected = plan(json!({
            "diff": "--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -1 +1 @@\n-let x = nil;\n+let x = 0;",
            "explanation": "Replace nil with zero to fix the crash."
        }));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::ProposePatch {
            task: json!({"id": "t2", "description": "apply fix"}),
            code_context: json!({"file": "src/auth.rs", "content": "let x = nil;"}),
            patch_policy: "minimal diff".into(),
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedPlan);
    }

    // ── StructureToolObservation ──────────────────────────────────────────────

    #[tokio::test]
    async fn structure_tool_observation_returns_assessment() {
        let expected = assessment(json!({
            "summary": "Tests passed: 5/5",
            "structured": {"passed": 5, "failed": 0}
        }));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::StructureToolObservation {
            task: json!({"id": "t3", "description": "run tests"}),
            raw_output: "test result: ok. 5 passed".into(),
            expected_observation: "{\"passed\": int, \"failed\": int}".into(),
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedAssessment);
    }

    // ── ProposeRecoveryOptions ────────────────────────────────────────────────

    #[tokio::test]
    async fn propose_recovery_options_returns_assessment() {
        let expected = assessment(json!({
            "options": [
                {"label": "retry", "description": "Retry the failed task"},
                {"label": "rollback", "description": "Roll back to last checkpoint"}
            ]
        }));
        let adapter = MockLlmAdapter::new(vec![expected.clone()]);
        let req = LlmRequest::ProposeRecoveryOptions {
            failure: "build failed: missing symbol".into(),
            known_context: json!({"checkpoint": "cp1"}),
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmProposedAssessment);
    }

    // ── Failure path ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn empty_adapter_returns_llm_failed_for_any_request() {
        let adapter = MockLlmAdapter::empty();
        let req = LlmRequest::ExtractIntent {
            text: "x".into(),
            allowed_intents: vec!["a".into()],
        };
        let event = adapter.invoke(req).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmFailed);
    }
}
