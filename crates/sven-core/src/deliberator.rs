// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reusable model↔tool agentic loop ("deliberation").
//!
//! [`Deliberator`] generalises the engine that drives [`crate::Agent`]'s
//! conversational turn: stream a model response, dispatch any native tool
//! calls in parallel (via [`ToolSlotManager`]), append the results, and repeat
//! until the model produces a final tool-free answer.  It is the shared kernel
//! the SDLC deliberation executor uses so that every HSM state can run a
//! state-scoped agentic loop without duplicating the streaming / tool-dispatch
//! machinery.
//!
//! # Cache-safety / append-only invariant
//!
//! The loop operates on a borrowed conversation **thread** (`&mut Vec<Message>`)
//! that it only ever *appends* to: the system role (once), the user
//! instruction, assistant text, assistant tool-calls, and tool results.  It
//! never rewrites earlier turns, so a provider's prompt cache stays valid
//! across deliberations on the same thread.
//!
//! # Differences from `Agent`
//!
//! `Agent` owns a [`crate::Session`] with token budgeting, calibration, mode /
//! model switching and full compaction.  Deliberations are short-lived,
//! state-scoped, and single-model, so [`Deliberator`] keeps only what they
//! need: streaming, parallel tools, `max_tool_rounds` wrap-up, tool-output
//! truncation and cancellation.  The two share the lower-level primitives
//! ([`ToolSlotManager`] and the inline-markup recovery helpers in
//! [`crate::agent`]).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use sven_model::{
    CompletionRequest, FunctionCall, Message, MessageContent, ModelProvider, ResponseEvent,
    ResponseFormat, Role, ToolSchema,
};
use sven_tools::ToolRegistry;

use crate::agent::{
    extract_inline_invoke_tool_calls, extract_inline_think_block, strip_think_wrappers,
};
use crate::compact::smart_truncate;
use crate::events::AgentEvent;
use crate::tool_slots::ToolSlotManager;

/// Maximum time to wait for the next streaming chunk before treating the
/// connection as stale.  Mirrors `agent::STREAM_CHUNK_TIMEOUT`.
const STREAM_CHUNK_TIMEOUT: Duration = Duration::from_secs(300);

/// Per-deliberation parameters.
pub struct DeliberationParams {
    /// Stable system-role framing (pushed once, when the thread is empty).
    pub system_role: String,
    /// The comprehensive per-turn command (always appended as a new user turn).
    pub instruction: String,
    /// Model-side tool schemas this deliberation may call (state-scoped subset).
    pub tools: Vec<ToolSchema>,
    /// Optional structured-output constraint forwarded to the model layer.
    pub response_format: Option<ResponseFormat>,
    /// Maximum model↔tool rounds before a final tool-free wrap-up turn.
    pub max_tool_rounds: u32,
    /// Per-tool-result token cap (smart-truncates large outputs before append).
    pub tool_result_token_cap: usize,
}

impl Default for DeliberationParams {
    fn default() -> Self {
        Self {
            system_role: String::new(),
            instruction: String::new(),
            tools: Vec::new(),
            response_format: None,
            max_tool_rounds: 16,
            tool_result_token_cap: 8_000,
        }
    }
}

/// Convert tool-registry schemas into model-API schemas (core tools first,
/// preserving the order produced by [`ToolRegistry::schemas_for_names`]).
#[must_use]
pub fn to_model_schemas(schemas: Vec<sven_tools::ToolSchema>) -> Vec<ToolSchema> {
    schemas
        .into_iter()
        .map(|s| ToolSchema {
            name: s.name,
            description: s.description,
            parameters: s.parameters,
            is_mcp: s.is_mcp,
        })
        .collect()
}

/// Drives one model↔tool agentic loop against a borrowed conversation thread.
pub struct Deliberator {
    model: Arc<dyn ModelProvider>,
    tools: Arc<ToolRegistry>,
}

impl Deliberator {
    /// Create a deliberator bound to a model provider and tool registry.
    #[must_use]
    pub fn new(model: Arc<dyn ModelProvider>, tools: Arc<ToolRegistry>) -> Self {
        Self { model, tools }
    }

    /// Run the agentic loop and return the model's final tool-free text (the
    /// structured decision).  `thread` is appended to in place (never rewritten).
    ///
    /// `tx` receives [`AgentEvent`]s for UI bridging; `cancel` fires to abort.
    ///
    /// # Errors
    ///
    /// Returns an error if a model call fails.  Cancellation returns `Ok` with
    /// whatever text was accumulated before the abort.
    pub async fn run(
        &self,
        thread: &mut Vec<Message>,
        params: DeliberationParams,
        tx: mpsc::Sender<AgentEvent>,
        mut cancel: oneshot::Receiver<()>,
    ) -> anyhow::Result<String> {
        // Append-only seeding: system role once, then the instruction turn.
        if thread.is_empty() && !params.system_role.is_empty() {
            thread.push(Message::system(&params.system_role));
        }
        thread.push(Message::user(&params.instruction));

        let core_tool_count = params.tools.iter().filter(|s| !s.is_mcp).count();
        let mut rounds = 0u32;
        let mut last_text = String::new();

        loop {
            // Cancellation check before each round.
            match cancel.try_recv() {
                Err(oneshot::error::TryRecvError::Empty) => {}
                _ => {
                    let _ = tx
                        .send(AgentEvent::Aborted {
                            partial_text: last_text.clone(),
                        })
                        .await;
                    return Ok(last_text);
                }
            }

            rounds += 1;
            let with_tools = rounds <= params.max_tool_rounds && !params.tools.is_empty();

            // On the wrap-up round, instruct the model to conclude with a
            // decision and stop calling tools.
            if rounds == params.max_tool_rounds + 1 {
                thread.push(Message::user(
                    "You have reached the maximum tool-call budget. Do not call any \
                     more tools. Respond now with your final structured decision per \
                     the required schema.",
                ));
            }

            let turn = tokio::select! {
                biased;
                _ = &mut cancel => None,
                result = self.stream_one_turn(
                    thread,
                    if with_tools { &params.tools } else { &[] },
                    core_tool_count,
                    params.response_format.clone(),
                    &tx,
                ) => Some(result),
            };

            let (text, slot_manager, had_tool_calls) = match turn {
                None => {
                    let _ = tx
                        .send(AgentEvent::Aborted {
                            partial_text: last_text.clone(),
                        })
                        .await;
                    return Ok(last_text);
                }
                Some(Err(e)) => {
                    let _ = tx.send(AgentEvent::Error(format!("{e:#}"))).await;
                    return Err(e);
                }
                Some(Ok(t)) => t,
            };

            if !text.is_empty() {
                last_text = text.clone();
                thread.push(Message::assistant(&text));
            }

            if !had_tool_calls {
                let _ = tx.send(AgentEvent::TurnComplete).await;
                return Ok(last_text);
            }

            // Await tool results (cancellable).
            let join_fut = slot_manager.join_all(&tx);
            tokio::pin!(join_fut);
            let results = tokio::select! {
                biased;
                _ = &mut cancel => {
                    let _ = tx
                        .send(AgentEvent::Aborted { partial_text: last_text.clone() })
                        .await;
                    return Ok(last_text);
                }
                res = &mut join_fut => res,
            };

            // Append assistant tool-call messages first (OpenAI wire order),
            // then the tool results — all append-only.
            for (tc, _) in &results {
                thread.push(Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tc.id.clone(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.args.to_string(),
                        },
                    },
                });
            }
            for (tc, output) in &results {
                let category = self.tools.output_category(&tc.name);
                let tool_msg = if output.has_images() {
                    use sven_model::ToolContentPart;
                    let parts: Vec<ToolContentPart> = output
                        .parts
                        .iter()
                        .map(|p| match p {
                            sven_tools::ToolOutputPart::Text(t) => ToolContentPart::Text {
                                text: smart_truncate(t, category, params.tool_result_token_cap),
                            },
                            sven_tools::ToolOutputPart::Image(url) => ToolContentPart::Image {
                                image_url: url.clone(),
                            },
                        })
                        .collect();
                    Message::tool_result_with_parts(&tc.id, parts)
                } else {
                    let content =
                        smart_truncate(&output.content, category, params.tool_result_token_cap);
                    Message::tool_result(&tc.id, &content)
                };
                thread.push(tool_msg);
            }
        }
    }

    /// Stream a single model turn, dispatching tool calls as their JSON args
    /// complete.  Returns `(text, slot_manager, had_tool_calls)`.
    async fn stream_one_turn(
        &self,
        thread: &[Message],
        tools: &[ToolSchema],
        core_tool_count: usize,
        response_format: Option<ResponseFormat>,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<(String, ToolSlotManager, bool)> {
        let modalities = self.model.input_modalities();
        let messages =
            sven_model::sanitize::strip_images_if_unsupported(thread.to_vec(), &modalities);

        let req = CompletionRequest {
            messages,
            tools: tools.to_vec(),
            stream: true,
            system_dynamic_suffix: None,
            cache_key: None,
            max_output_tokens_override: None,
            core_tool_count,
            response_format,
        };

        let mut stream = self.model.complete(req).await?;

        let mut full_text = String::new();
        let mut slot_manager = ToolSlotManager::new(Arc::clone(&self.tools));
        let mut thinking_buf = String::new();

        loop {
            let maybe_event = tokio::time::timeout(STREAM_CHUNK_TIMEOUT, stream.next())
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "model stream idle for >{} s - stale connection",
                        STREAM_CHUNK_TIMEOUT.as_secs()
                    )
                })?;
            let event = match maybe_event {
                None => break,
                Some(e) => e?,
            };
            match event {
                ResponseEvent::ThinkingDelta(delta) => {
                    thinking_buf.push_str(&delta);
                    let _ = tx.send(AgentEvent::ThinkingDelta(delta)).await;
                }
                ResponseEvent::TextDelta(delta) if !delta.is_empty() => {
                    if !thinking_buf.is_empty() {
                        let content = std::mem::take(&mut thinking_buf);
                        let _ = tx
                            .send(AgentEvent::ThinkingComplete(strip_think_wrappers(content)))
                            .await;
                    }
                    full_text.push_str(&delta);
                    let _ = tx.send(AgentEvent::TextDelta(delta)).await;
                }
                ResponseEvent::ToolCall {
                    index,
                    id,
                    name,
                    arguments,
                } => {
                    if let Some(tc) = slot_manager.feed(index, &id, &name, &arguments) {
                        let _ = tx.send(AgentEvent::ToolCallStarted(tc)).await;
                    }
                }
                ResponseEvent::Done => {
                    if !thinking_buf.is_empty() {
                        let content = std::mem::take(&mut thinking_buf);
                        let _ = tx
                            .send(AgentEvent::ThinkingComplete(strip_think_wrappers(content)))
                            .await;
                    }
                    break;
                }
                ResponseEvent::Error(e) => {
                    warn!("model stream error: {e}");
                }
                _ => {}
            }
        }

        // Reclassify an all-thinking text turn as thinking-only.
        if !full_text.is_empty() && thinking_buf.is_empty() {
            if let Some(inline_think) = extract_inline_think_block(&full_text) {
                let _ = tx.send(AgentEvent::ThinkingComplete(inline_think)).await;
                full_text.clear();
            }
        }

        for tc in slot_manager.finalize_remaining() {
            let _ = tx.send(AgentEvent::ToolCallStarted(tc)).await;
        }

        // Anthropic-style <invoke> fallback for models that emit XML tool calls.
        if slot_manager.is_empty() && full_text.contains("<invoke ") {
            let (cleaned, invoke_calls) = extract_inline_invoke_tool_calls(&full_text);
            if !invoke_calls.is_empty() {
                full_text = cleaned;
                for (i, tc) in invoke_calls.into_iter().enumerate() {
                    let _ = tx.send(AgentEvent::ToolCallStarted(tc.clone())).await;
                    slot_manager.insert_call(i as u32, tc);
                }
            }
        }

        if !full_text.is_empty() {
            let _ = tx.send(AgentEvent::TextComplete(full_text.clone())).await;
        }

        let had_tool_calls = !slot_manager.is_empty();
        Ok((full_text, slot_manager, had_tool_calls))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use sven_tools::{policy::ApprovalPolicy, tool::Tool, ToolCall, ToolOutput};

    /// Streams a scripted set of turns: each call pops the next `Vec` of events.
    struct ScriptProvider {
        turns: std::sync::Mutex<std::collections::VecDeque<Vec<ResponseEvent>>>,
    }

    impl ScriptProvider {
        fn new(turns: Vec<Vec<ResponseEvent>>) -> Self {
            Self {
                turns: std::sync::Mutex::new(turns.into_iter().collect()),
            }
        }
    }

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
            let turn = self.turns.lock().unwrap().pop_front().unwrap_or_default();
            let events: Vec<anyhow::Result<ResponseEvent>> = turn.into_iter().map(Ok).collect();
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echoes args"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            ToolOutput::ok(&call.id, format!("echoed:{}", call.args))
        }
    }

    fn registry() -> Arc<ToolRegistry> {
        let mut r = ToolRegistry::new();
        r.register(EchoTool);
        Arc::new(r)
    }

    #[tokio::test]
    async fn single_turn_returns_text_and_appends_only() {
        let provider = Arc::new(ScriptProvider::new(vec![vec![
            ResponseEvent::TextDelta("{\"status\":\"proceed\"}".into()),
            ResponseEvent::Done,
        ]]));
        let delib = Deliberator::new(provider, registry());
        let mut thread = Vec::new();
        let (tx, _rx) = mpsc::channel(64);
        let (_ctx, crx) = oneshot::channel();
        let out = delib
            .run(
                &mut thread,
                DeliberationParams {
                    system_role: "role".into(),
                    instruction: "decide".into(),
                    ..Default::default()
                },
                tx,
                crx,
            )
            .await
            .unwrap();
        assert_eq!(out, "{\"status\":\"proceed\"}");
        // system + user(instruction) + assistant(text)
        assert_eq!(thread.len(), 3);
        assert_eq!(thread[0].role, Role::System);
        assert_eq!(thread[1].role, Role::User);
        assert_eq!(thread[2].role, Role::Assistant);
    }

    #[tokio::test]
    async fn tool_loop_appends_calls_and_results() {
        let provider = Arc::new(ScriptProvider::new(vec![
            // Turn 1: one tool call.
            vec![
                ResponseEvent::ToolCall {
                    index: 0,
                    id: "c1".into(),
                    name: "echo".into(),
                    arguments: "{\"a\":1}".into(),
                },
                ResponseEvent::Done,
            ],
            // Turn 2: final decision.
            vec![
                ResponseEvent::TextDelta("{\"status\":\"proceed\"}".into()),
                ResponseEvent::Done,
            ],
        ]));
        let delib = Deliberator::new(provider, registry());
        let mut thread = Vec::new();
        let (tx, _rx) = mpsc::channel(256);
        let (_ctx, crx) = oneshot::channel();
        let tools = vec![ToolSchema {
            name: "echo".into(),
            description: "echoes".into(),
            parameters: json!({"type":"object"}),
            is_mcp: false,
        }];
        let out = delib
            .run(
                &mut thread,
                DeliberationParams {
                    system_role: "role".into(),
                    instruction: "decide".into(),
                    tools,
                    ..Default::default()
                },
                tx,
                crx,
            )
            .await
            .unwrap();
        assert_eq!(out, "{\"status\":\"proceed\"}");
        // system, user, assistant(toolcall), tool(result), assistant(final)
        assert_eq!(thread.len(), 5);
        assert!(matches!(
            thread[2].content,
            MessageContent::ToolCall { .. }
        ));
        assert!(matches!(
            thread[3].content,
            MessageContent::ToolResult { .. }
        ));
    }
}
