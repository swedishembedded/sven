//! The [`LlmAdapter`] trait and its production implementation.
//!
//! An adapter converts a typed [`LlmRequest`] into a
//! [`sven_model::CompletionRequest`], calls the model provider, accumulates
//! the streamed response, and maps the parsed JSON into the correct
//! [`sven_hsm::Event`] variant.
//!
//! The LLM **never** names a tool to run or a state to enter; it only
//! returns structured data proposals in JSON.

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use sven_hsm::{Event, ObservationSink, UiEvent};
use sven_model::{CompletionRequest, Message, ModelProvider, ResponseEvent};

use crate::error::LlmError;
use crate::request::LlmRequest;
use crate::response::{
    AnswerInterpretation, CandidatePlan, ClarifyingQuestion, CompletenessAssessment, Constraints,
    IntentExtraction, PatchProposal, ProblemStatement, RecoveryOptions, TaskDecomposition,
    ToolObservation,
};

/// Converts a typed [`LlmRequest`] into a single [`sven_hsm::Event`].
///
/// Implementations must be `Send + Sync` so they can be held behind an `Arc`
/// and shared across executor tasks.
#[async_trait]
pub trait LlmAdapter: Send + Sync {
    /// Invoke the LLM with the given request and return the resulting event.
    ///
    /// If `obs` is provided, streaming text/thinking deltas and usage events
    /// are forwarded onto the outward observation plane as [`UiEvent`]s so
    /// frontends can render progress in real time.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError`] if the provider fails or the response cannot be
    /// parsed into the expected shape.
    async fn invoke(
        &self,
        req: LlmRequest,
        obs: Option<&ObservationSink>,
    ) -> Result<Event, LlmError>;
}

// ── DefaultLlmAdapter ─────────────────────────────────────────────────────────

/// Production adapter that drives a real [`ModelProvider`].
pub struct DefaultLlmAdapter {
    provider: Box<dyn ModelProvider>,
    /// Optional outward observation sink. When present, streamed text /
    /// thinking deltas and usage are forwarded as [`UiEvent`]s while the
    /// completion is in flight, instead of being silently discarded.
    obs: Option<ObservationSink>,
}

impl DefaultLlmAdapter {
    /// Wraps any [`ModelProvider`] implementation.
    pub fn new(provider: Box<dyn ModelProvider>) -> Self {
        Self {
            provider,
            obs: None,
        }
    }

    /// Like [`new`](Self::new) but forwards streamed deltas / usage to `obs`.
    ///
    /// This is what makes the typed-JSON path observable: every `TextDelta`,
    /// `ThinkingDelta`, and `Usage` event the provider streams is mirrored
    /// onto the outward plane as a [`UiEvent`], so frontends can render the
    /// model thinking in real time even for structured SDLC requests.
    pub fn with_observation(provider: Box<dyn ModelProvider>, obs: ObservationSink) -> Self {
        Self {
            provider,
            obs: Some(obs),
        }
    }
}

#[async_trait]
impl LlmAdapter for DefaultLlmAdapter {
    async fn invoke(
        &self,
        req: LlmRequest,
        obs: Option<&ObservationSink>,
    ) -> Result<Event, LlmError> {
        let (system_prompt, user_prompt) = build_prompt(&req);
        let cr = CompletionRequest {
            messages: vec![Message::system(system_prompt), Message::user(user_prompt)],
            stream: true,
            ..Default::default()
        };
        // Caller-supplied obs takes precedence over the stored one so that the
        // executor (which knows the current observation sink) can inject it at
        // call-time even when the adapter was constructed without one.
        let effective_obs = obs.or(self.obs.as_ref());
        let raw = accumulate_stream(self.provider.as_ref(), cr, effective_obs).await?;
        parse_response(&req, &raw)
    }
}

// ── Prompt construction ───────────────────────────────────────────────────────

/// Returns `(system_prompt, user_prompt)` for each request variant.
fn build_prompt(req: &LlmRequest) -> (String, String) {
    match req {
        LlmRequest::ExtractIntent {
            text,
            allowed_intents,
        } => {
            let system = format!(
                "You are an intent classifier. Respond with a JSON object with exactly these \
                 fields: {{\"intent\": \"<one of the allowed intents>\", \"confidence\": <float \
                 0.0-1.0>}}. Allowed intents: {}.",
                allowed_intents.join(", ")
            );
            let user = format!("Classify the intent of this text:\n{text}");
            (system, user)
        }

        LlmRequest::ExtractProblemStatement {
            intent,
            known_context,
        } => {
            let system = "You are a requirements analyst. Respond with a JSON object: \
                          {\"statement\": \"<concise problem statement>\", \
                          \"keywords\": [\"<keyword>\", ...]}."
                .into();
            let user = format!(
                "Intent: {intent}\nKnown context: {}\n\nWrite a clear problem statement.",
                serde_json::to_string_pretty(known_context)
                    .unwrap_or_else(|_| known_context.to_string())
            );
            (system, user)
        }

        LlmRequest::ExtractConstraints { known_context } => {
            let system = "You are a requirements analyst. Respond with a JSON object: \
                          {\"items\": [\"<constraint>\", ...]}. \
                          List all hard constraints implied by the context."
                .into();
            let user = format!(
                "Known context:\n{}",
                serde_json::to_string_pretty(known_context)
                    .unwrap_or_else(|_| known_context.to_string())
            );
            (system, user)
        }

        LlmRequest::AssessCompleteness {
            known_context,
            required_fields,
        } => {
            let system = "You are a completeness assessor. Respond with a JSON object following \
                          this schema: {\"status\": \"enough\" | {\"status\": \"missing\", \
                          \"fields\": [{\"field\": \"name\", \"reason\": \"why\"}]} | \
                          {\"status\": \"blocked\", \"reason\": \"why\"}}."
                .into();
            let user = format!(
                "Required fields: {}\nKnown context:\n{}\n\nAssess completeness.",
                required_fields.join(", "),
                serde_json::to_string_pretty(known_context)
                    .unwrap_or_else(|_| known_context.to_string())
            );
            (system, user)
        }

        LlmRequest::GenerateClarifyingQuestion {
            missing,
            known_context,
            question_policy,
        } => {
            let system = format!(
                "You are a helpful assistant gathering requirements. Respond with a JSON object: \
                 {{\"question\": \"<question text>\", \"hint\": \"<hint or null>\"}}. \
                 Policy: {question_policy}."
            );
            let user = format!(
                "Still missing: {}\nKnown context:\n{}",
                missing.join(", "),
                serde_json::to_string_pretty(known_context)
                    .unwrap_or_else(|_| known_context.to_string())
            );
            (system, user)
        }

        LlmRequest::GenerateResponse { intent } => {
            let system = "You are Sven, a helpful and concise coding assistant. \
                          Respond to the user in natural language. Do not wrap your \
                          answer in JSON or code fences unless the user asked for code."
                .into();
            let user = format!(
                "Context / interpreted intent:\n{}",
                serde_json::to_string_pretty(intent).unwrap_or_else(|_| intent.to_string())
            );
            (system, user)
        }

        LlmRequest::InterpretUserAnswer {
            question,
            answer,
            expected_answer_shape,
        } => {
            let system = format!(
                "You are an answer interpreter. Respond with a JSON object: \
                 {{\"extracted\": <structured data matching: {expected_answer_shape}>, \
                 \"confidence\": <float 0.0-1.0>}}."
            );
            let user = format!("Question: {question}\nUser answer: {answer}");
            (system, user)
        }

        LlmRequest::GenerateCandidatePlan {
            known_context,
            planning_policy,
        } => {
            let system = format!(
                "You are a planning assistant. Respond with a JSON object: \
                 {{\"title\": \"<plan title>\", \"steps\": [\"<step>\", ...], \
                 \"risk\": \"low|medium|high\"}}. Policy: {planning_policy}."
            );
            let user = format!(
                "Known context:\n{}",
                serde_json::to_string_pretty(known_context)
                    .unwrap_or_else(|_| known_context.to_string())
            );
            (system, user)
        }

        LlmRequest::DecomposeIntoTasks {
            selected_plan,
            task_policy,
        } => {
            let system = format!(
                "You are a task decomposer. Respond with a JSON object: \
                 {{\"tasks\": [{{\"id\": \"t1\", \"description\": \"...\", \
                 \"tool\": \"<name or null>\", \"args\": {{}}}}]}}. \
                 Policy: {task_policy}."
            );
            let user = format!(
                "Plan:\n{}",
                serde_json::to_string_pretty(selected_plan)
                    .unwrap_or_else(|_| selected_plan.to_string())
            );
            (system, user)
        }

        LlmRequest::ProposePatch {
            task,
            code_context,
            patch_policy,
        } => {
            let system = format!(
                "You are a code patching assistant. Respond with a JSON object: \
                 {{\"diff\": \"<unified diff>\", \"explanation\": \"<plain-language rationale>\"}}. \
                 Policy: {patch_policy}."
            );
            let user = format!(
                "Task:\n{}\n\nCode context:\n{}",
                serde_json::to_string_pretty(task).unwrap_or_else(|_| task.to_string()),
                serde_json::to_string_pretty(code_context)
                    .unwrap_or_else(|_| code_context.to_string())
            );
            (system, user)
        }

        LlmRequest::StructureToolObservation {
            task,
            raw_output,
            expected_observation,
        } => {
            let system = format!(
                "You are a tool output parser. Respond with a JSON object: \
                 {{\"summary\": \"<one sentence>\", \"structured\": \
                 <structured data matching: {expected_observation}>}}."
            );
            let user = format!(
                "Task:\n{}\n\nRaw tool output:\n{raw_output}",
                serde_json::to_string_pretty(task).unwrap_or_else(|_| task.to_string())
            );
            (system, user)
        }

        LlmRequest::ProposeRecoveryOptions {
            failure,
            known_context,
        } => {
            let system = "You are a failure recovery advisor. Respond with a JSON object: \
                          {\"options\": [{\"label\": \"<label>\", \
                          \"description\": \"<description>\"}]}."
                .into();
            let user = format!(
                "Failure: {failure}\nKnown context:\n{}",
                serde_json::to_string_pretty(known_context)
                    .unwrap_or_else(|_| known_context.to_string())
            );
            (system, user)
        }
    }
}

// ── Stream accumulation ───────────────────────────────────────────────────────

/// Drains a completion stream, collecting `TextDelta` chunks into a single
/// string.
///
/// When `obs` is `Some`, the streamed deltas are *also* forwarded onto the
/// outward observation plane as [`UiEvent`]s (text, thinking, usage) so the
/// caller can render them live. Previously these events were discarded, which
/// meant the typed-JSON path produced no streaming UX at all.
async fn accumulate_stream(
    provider: &dyn ModelProvider,
    req: CompletionRequest,
    obs: Option<&ObservationSink>,
) -> Result<String, LlmError> {
    let mut stream = provider.complete(req).await?;
    let mut text = String::new();
    let mut thinking = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        match event {
            ResponseEvent::TextDelta(delta) => {
                if let Some(obs) = obs {
                    if !thinking.is_empty() {
                        obs.emit(UiEvent::ThinkingComplete(std::mem::take(&mut thinking)));
                    }
                    obs.emit(UiEvent::TextDelta(delta.clone()));
                }
                text.push_str(&delta);
            }
            ResponseEvent::ThinkingDelta(delta) => {
                if let Some(obs) = obs {
                    obs.emit(UiEvent::ThinkingDelta(delta.clone()));
                }
                thinking.push_str(&delta);
            }
            ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                cost_usd,
            } => {
                if let Some(obs) = obs {
                    obs.emit(UiEvent::TokenUsage {
                        input: input_tokens,
                        output: output_tokens,
                        cache_read: cache_read_tokens,
                        cache_write: cache_write_tokens,
                        cache_read_total: cache_read_tokens,
                        cache_write_total: cache_write_tokens,
                        max_tokens: 0,
                        max_output_tokens: 0,
                        cost_usd,
                    });
                }
            }
            ResponseEvent::Done => break,
            ResponseEvent::Error(e) => {
                tracing::warn!(error = %e, "LLM stream error (non-fatal)");
            }
            _ => {}
        }
    }
    if let Some(obs) = obs {
        if !thinking.is_empty() {
            obs.emit(UiEvent::ThinkingComplete(thinking));
        }
        if !text.is_empty() {
            obs.emit(UiEvent::TextComplete(text.clone()));
        }
    }
    Ok(text)
}

// ── Response parsing → Event mapping ─────────────────────────────────────────

/// Parses the accumulated JSON text from the LLM into the correct
/// [`sven_hsm::Event`] variant.
fn parse_response(req: &LlmRequest, raw: &str) -> Result<Event, LlmError> {
    // Strip Markdown code fences if the LLM wrapped the JSON.
    let json_str = strip_code_fences(raw);
    let kind = req.kind_name();

    let to_assessment = |v: Value| Event::LlmProposedAssessment { assessment: v };
    let to_plan = |v: Value| Event::LlmProposedPlan { plan: v };

    let parse_err = |e: serde_json::Error| LlmError::ParseFailure {
        request_kind: kind,
        detail: format!("{e} (raw: {json_str})"),
    };

    match req {
        LlmRequest::ExtractIntent { .. } => {
            let r: IntentExtraction = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(to_assessment(
                serde_json::to_value(r).expect("IntentExtraction serialisation infallible"),
            ))
        }

        LlmRequest::ExtractProblemStatement { .. } => {
            let r: ProblemStatement = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(to_assessment(
                serde_json::to_value(r).expect("ProblemStatement serialisation infallible"),
            ))
        }

        LlmRequest::ExtractConstraints { .. } => {
            let r: Constraints = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(to_assessment(
                serde_json::to_value(r).expect("Constraints serialisation infallible"),
            ))
        }

        LlmRequest::AssessCompleteness { .. } => {
            let r: CompletenessAssessment = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(to_assessment(
                serde_json::to_value(r).expect("CompletenessAssessment serialisation infallible"),
            ))
        }

        LlmRequest::GenerateClarifyingQuestion { .. } => {
            let r: ClarifyingQuestion = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(Event::LlmProposedResponse { text: r.question })
        }

        // The conversational response is plain assistant text, not JSON: return
        // the raw (un-fenced) model output directly.
        LlmRequest::GenerateResponse { .. } => Ok(Event::LlmProposedResponse {
            text: raw.trim().to_string(),
        }),

        LlmRequest::InterpretUserAnswer { .. } => {
            let r: AnswerInterpretation = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(to_assessment(
                serde_json::to_value(r).expect("AnswerInterpretation serialisation infallible"),
            ))
        }

        LlmRequest::GenerateCandidatePlan { .. } => {
            let r: CandidatePlan = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(to_plan(
                serde_json::to_value(r).expect("CandidatePlan serialisation infallible"),
            ))
        }

        LlmRequest::DecomposeIntoTasks { .. } => {
            let r: TaskDecomposition = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(to_plan(
                serde_json::to_value(r).expect("TaskDecomposition serialisation infallible"),
            ))
        }

        LlmRequest::ProposePatch { .. } => {
            let r: PatchProposal = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(to_plan(
                serde_json::to_value(r).expect("PatchProposal serialisation infallible"),
            ))
        }

        LlmRequest::StructureToolObservation { .. } => {
            let r: ToolObservation = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(to_assessment(
                serde_json::to_value(r).expect("ToolObservation serialisation infallible"),
            ))
        }

        LlmRequest::ProposeRecoveryOptions { .. } => {
            let r: RecoveryOptions = serde_json::from_str(json_str).map_err(parse_err)?;
            Ok(to_assessment(
                serde_json::to_value(r).expect("RecoveryOptions serialisation infallible"),
            ))
        }
    }
}

/// Strip optional Markdown code fences (` ```json ... ``` `) from LLM output.
fn strip_code_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("```json").unwrap_or(s);
    let s = s.strip_prefix("```").unwrap_or(s);
    let s = s.strip_suffix("```").unwrap_or(s);
    s.trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Streaming forwarding ──────────────────────────────────────────────────

    /// Minimal in-memory provider that replays a fixed event script.
    struct ScriptProvider(Vec<ResponseEvent>);

    #[async_trait]
    impl ModelProvider for ScriptProvider {
        fn name(&self) -> &str {
            "script"
        }
        fn model_name(&self) -> &str {
            "script"
        }
        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> anyhow::Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>,
        > {
            let events: Vec<anyhow::Result<ResponseEvent>> =
                self.0.iter().cloned().map(Ok).collect();
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn accumulate_stream_forwards_deltas_to_observation_sink() {
        let provider = ScriptProvider(vec![
            ResponseEvent::ThinkingDelta("hmm".into()),
            ResponseEvent::TextDelta("pong".into()),
            ResponseEvent::Done,
        ]);
        let obs = ObservationSink::new(16);
        let mut rx = obs.subscribe();

        let text = accumulate_stream(&provider, CompletionRequest::default(), Some(&obs))
            .await
            .unwrap();
        assert_eq!(text, "pong");

        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push(ev);
        }
        assert!(seen.contains(&UiEvent::ThinkingDelta("hmm".into())));
        assert!(seen.contains(&UiEvent::ThinkingComplete("hmm".into())));
        assert!(seen.contains(&UiEvent::TextDelta("pong".into())));
        assert!(seen.contains(&UiEvent::TextComplete("pong".into())));
    }

    #[tokio::test]
    async fn accumulate_stream_without_sink_still_collects_text() {
        let provider = ScriptProvider(vec![
            ResponseEvent::TextDelta("a".into()),
            ResponseEvent::TextDelta("b".into()),
            ResponseEvent::Done,
        ]);
        let text = accumulate_stream(&provider, CompletionRequest::default(), None)
            .await
            .unwrap();
        assert_eq!(text, "ab");
    }

    #[test]
    fn strip_fences_removes_json_fence() {
        let raw = "```json\n{\"intent\": \"bugfix\", \"confidence\": 0.9}\n```";
        let stripped = strip_code_fences(raw);
        assert_eq!(stripped, "{\"intent\": \"bugfix\", \"confidence\": 0.9}");
    }

    #[test]
    fn strip_fences_passthrough_plain_json() {
        let raw = "{\"intent\": \"bugfix\", \"confidence\": 0.9}";
        assert_eq!(strip_code_fences(raw), raw);
    }

    #[test]
    fn parse_extract_intent_response() {
        let req = LlmRequest::ExtractIntent {
            text: "fix the crash".into(),
            allowed_intents: vec!["bugfix".into()],
        };
        let raw = r#"{"intent": "bugfix", "confidence": 0.95}"#;
        let event = parse_response(&req, raw).unwrap();
        assert!(matches!(event, Event::LlmProposedAssessment { .. }));
    }

    #[test]
    fn parse_generate_clarifying_question_response() {
        let req = LlmRequest::GenerateClarifyingQuestion {
            missing: vec!["repo_url".into()],
            known_context: Value::Null,
            question_policy: "brief".into(),
        };
        let raw = r#"{"question": "What is the repository URL?", "hint": null}"#;
        let event = parse_response(&req, raw).unwrap();
        match event {
            Event::LlmProposedResponse { text } => {
                assert_eq!(text, "What is the repository URL?");
            }
            other => panic!("expected LlmProposedResponse, got {other:?}"),
        }
    }

    #[test]
    fn parse_generate_candidate_plan_response() {
        let req = LlmRequest::GenerateCandidatePlan {
            known_context: Value::Null,
            planning_policy: "safe".into(),
        };
        let raw = r#"{"title": "Plan A", "steps": ["step 1"], "risk": "low"}"#;
        let event = parse_response(&req, raw).unwrap();
        assert!(matches!(event, Event::LlmProposedPlan { .. }));
    }
}
