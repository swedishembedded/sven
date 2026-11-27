// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Single-turn streaming primitive: stream one model response and collect
//! proposed tool calls **without executing them**.
//!
//! [`stream_turn`] is the reusable kernel-friendly building block that
//! `TurnExecutor` (`sven-executors`) calls.  It is accumulation-only: JSON arguments are
//! assembled slot-by-slot as they arrive in the stream, and the finalised
//! [`ToolCall`] list is returned to the caller.  No tool is ever dispatched
//! here; that responsibility belongs to the kernel (via `Effect::CallTool`).
//!
//! # UiEvent bridging
//!
//! The function forwards live progress onto `tx` as `AgentEvent`s so callers
//! can bridge them to the TUI outward plane while the stream is in flight.
//! `TextDelta`, `ThinkingDelta`, `ThinkingComplete`, `ToolCallStarted`,
//! `TokenUsage`, and `TextComplete` are all emitted.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::warn;

use sven_model::{
    CompletionRequest, Message, ModelProvider, PromptManifest, ResponseEvent, ResponseFormat,
    SamplingParams, ToolSchema,
};
use sven_tool_api::ToolCall;

use crate::thinking_budget::estimate_tokens;
pub use crate::thinking_budget::{
    set_thinking_budget_override, thinking_budget_override, ThinkingBudget, REASONING_LOOP_PREFIX,
    TOOL_CALL_LOOP_PREFIX,
};

/// Convert tool-registry schemas into model-API schemas, preserving the order
/// produced by [`sven_tool_registry::ToolRegistry::schemas_for_names`].
#[must_use]
pub fn to_model_schemas(schemas: Vec<sven_tool_registry::ToolSchema>) -> Vec<ToolSchema> {
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

use crate::ledger_recorder::LedgerRecorder;
use crate::repetition::{RepetitionWatch, TOOL_CALL_REPEATS};
use crate::sampled_checks::{check_sampler_support, check_top_logprobs};
use crate::stall::{StallLimits, StallWatch};
use crate::tool_slots::{unique_call_id, AccumSlot};
use sven_vocab::{
    ArgError, ReplyFinish, ReportedUsage, Sampled, SampledCall, SampledReply,
    SessionEvent as AgentEvent,
};

pub use crate::aborted::{AbortedError, Repetition};

/// Callback that resolves a model string (e.g. `"anthropic/claude-opus"`) to a
/// live [`ModelProvider`].  Provided by the bootstrap layer so that the turn
/// executor (`sven-executors`) can switch models mid-turn without depending
/// on the full the configuration.
pub type ModelResolver =
    std::sync::Arc<dyn Fn(&str) -> anyhow::Result<std::sync::Arc<dyn ModelProvider>> + Send + Sync>;

/// How a run asks the model to decode: the sampling controls, and whether the
/// sampled token ids and log-probabilities are wanted back. Part of what makes
/// a recorded trajectory reproducible and usable as training data.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Decoding {
    pub sampling: SamplingParams,
    pub want_logprobs: bool,
    /// With `want_logprobs`: the number of most probable tokens per position to ask for (see
    /// `CompletionRequest::top_logprobs`).
    pub top_logprobs: Option<u32>,
    /// With `want_logprobs`: the size of the sampler support to ask for (see `CompletionRequest::sampler_support`).
    pub sampler_support: Option<u32>,
}

/// Limits on one streamed model turn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnLimits {
    /// Bounds a runaway reasoning loop - see [`ThinkingBudget`].
    pub thinking: ThinkingBudget,
    /// Longest silence between two stream chunks before the connection is
    /// declared stale and the turn fails. `None` takes the default, which is
    /// larger for a local server (see [`ModelProvider::is_local`]) because a
    /// CPU prefill of a long prompt can legitimately stay silent for minutes
    /// before its first token.
    pub stream_idle: Option<Duration>,
    /// Limits on a server that connects but produces nothing - see
    /// [`StallLimits`].
    pub stall: StallLimits,
    /// Most output tokens one response may ask for, below whatever the model
    /// itself allows. `None` leaves the model's own cap in place.
    pub max_output_tokens: Option<u32>,
    /// Sampling controls sent with every request of the turn.
    pub sampling: SamplingParams,
    /// Reasoning switched on or off for every request of the turn (see
    /// `CompletionRequest::enable_thinking`).
    pub enable_thinking: Option<bool>,
    /// Whether the model may call the offered tools.
    pub tool_choice: sven_model::ToolChoice,
    /// The most wall-clock time one response may take, whole.
    pub deadline: Option<Duration>,
    /// Ask the provider for the sampled token ids and log-probabilities;
    /// they come back in [`AgentEvent::ResponseSampled`].
    pub want_logprobs: bool,
    /// With `want_logprobs`: how many most probable tokens per generated position to ask for and record
    /// ([`AgentEvent::ResponseSampled`]); a response without them fails the turn.
    pub top_logprobs: Option<u32>,
    /// With `want_logprobs`: the sampler support size to ask for and record; a response without it fails the turn.
    pub sampler_support: Option<u32>,
}

impl Default for TurnLimits {
    fn default() -> Self {
        ThinkingBudget::default().into()
    }
}

impl From<ThinkingBudget> for TurnLimits {
    fn from(thinking: ThinkingBudget) -> Self {
        Self {
            thinking,
            stream_idle: None,
            stall: StallLimits::default(),
            max_output_tokens: None,
            sampling: SamplingParams::default(),
            enable_thinking: None,
            tool_choice: sven_model::ToolChoice::Auto,
            deadline: None,
            want_logprobs: false,
            top_logprobs: None,
            sampler_support: None,
        }
    }
}

impl TurnLimits {
    /// The longest silence between two stream chunks, in seconds, before the
    /// connection is declared stale. `None` or `0` selects the default for
    /// the provider's locality.
    #[must_use]
    pub fn with_stream_idle_secs(mut self, secs: Option<u64>) -> Self {
        self.stream_idle = secs.filter(|&secs| secs > 0).map(Duration::from_secs);
        self
    }

    /// Bounds the wait for a server that connects but produces nothing.
    #[must_use]
    pub fn with_stall_limits(mut self, stall: StallLimits) -> Self {
        self.stall = stall;
        self
    }

    /// Caps each response at `tokens` output tokens.
    #[must_use]
    pub fn with_max_output_tokens(mut self, tokens: Option<u32>) -> Self {
        self.max_output_tokens = tokens;
        self
    }

    /// Sends `sampling` with every request and, when `want_logprobs`, asks
    /// for the sampled tokens back.
    #[must_use]
    pub fn with_decoding(mut self, decoding: Decoding) -> Self {
        self.sampling = decoding.sampling;
        self.want_logprobs = decoding.want_logprobs;
        self.top_logprobs = decoding.top_logprobs;
        self.sampler_support = decoding.sampler_support;
        self
    }

    /// [`Self::output_budget`], saying whether the window's room is what
    /// bounds it ([`sven_model::budget::output_limit`]).
    #[must_use]
    pub fn output_limit(
        &self,
        context_window: Option<u32>,
        model_cap: Option<u32>,
        prompt_tokens: usize,
    ) -> sven_model::budget::OutputLimit {
        let room = sven_model::budget::output_limit(context_window, model_cap, prompt_tokens);
        let tokens = self.cap_output(room.tokens);
        sven_model::budget::OutputLimit {
            tokens,
            is_room: room.is_room && tokens == room.tokens,
        }
    }

    /// The output tokens one request may ask for: what the prompt leaves of
    /// the context window under `model_cap`
    /// (`sven_model::budget::dynamic_output_budget`), held to this limit.
    #[must_use]
    pub fn output_budget(
        &self,
        context_window: Option<u32>,
        model_cap: Option<u32>,
        prompt_tokens: usize,
    ) -> Option<u32> {
        self.cap_output(sven_model::budget::dynamic_output_budget(
            context_window,
            model_cap,
            prompt_tokens,
        ))
    }

    /// The output-token cap for a model whose own cap - or the budget worked
    /// out for one request - is `model_cap`: the smaller of the two, or
    /// whichever one is set. The limit applies even when nothing is known
    /// about the model.
    #[must_use]
    pub fn cap_output(&self, model_cap: Option<u32>) -> Option<u32> {
        match (model_cap, self.max_output_tokens) {
            (Some(model), Some(limit)) => Some(model.min(limit)),
            (model, limit) => model.or(limit),
        }
    }
}

/// What one streamed model turn produced.
#[derive(Debug)]
pub struct StreamedTurn {
    /// The reply exactly as it arrived: filed in the history unchanged.
    pub reply: SampledReply,
    /// Each call of the reply as its tool runs it, **not executed**, in the
    /// reply's order: the parsed arguments, or `null` for a call that cannot
    /// run (see `arg_errors`).
    pub tool_calls: Vec<ToolCall>,
    /// Why a call cannot run, by call id: its arguments are not a JSON object,
    /// or the output limit cut them.
    pub arg_errors: HashMap<String, ArgError>,
    /// The provider stopped the reply at its output-token limit, so the text
    /// (and any call) may be cut off.
    pub truncated: bool,
    /// Estimated tokens the model spent thinking in this reply (chars/4).
    pub thinking_tokens: u32,
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Stream a single model turn and collect all proposed tool calls (accumulation-only).
///
/// Returns the turn's text and proposed tool calls, plus the raw arguments of
/// any call the model garbled beyond repair (see [`StreamedTurn`]).
///
/// `tx` receives [`AgentEvent`]s for UI bridging while the stream runs.
///
/// `output_limit` is the request's output-token limit (see
/// [`TurnLimits::output_limit`] for how callers typically compute it - scaled
/// to the actual prompt size rather than a fixed reservation); an
/// `Option<u32>` is a limit that was set. `None` tokens let the provider apply
/// its own built-in default.
///
/// `limits` bounds the turn - see [`TurnLimits`]. A stream silent for longer
/// than `limits.stream_idle` fails the turn, and so does a server that
/// produces no real output within `limits.stall` (keepalives do not count);
/// while it is silent an [`AgentEvent::ModelWaiting`] goes out every few
/// seconds. When either thinking cap is
/// exceeded this returns `Err` wrapping an
/// [`AbortedError`], which callers must route through the same
/// `UserCancelled`/`Aborted` handling as a user-initiated cancel, not a
/// genuine failure.
///
/// # Errors
///
/// Returns an error if the model API call fails, the server stalls past one
/// of its limits (see [`StallLimits`]), or the thinking budget is exceeded.
#[allow(clippy::too_many_arguments)]
pub async fn stream_turn(
    model: &dyn ModelProvider,
    messages: Vec<Message>,
    tools: Vec<ToolSchema>,
    cache_key: Option<String>,
    dynamic_suffix: Option<String>,
    response_format: Option<ResponseFormat>,
    output_limit: impl Into<sven_model::budget::OutputLimit>,
    limits: impl Into<TurnLimits>,
    tx: &mpsc::Sender<AgentEvent>,
) -> anyhow::Result<StreamedTurn> {
    stream_turn_attributed(
        model,
        messages,
        tools,
        cache_key,
        dynamic_suffix,
        response_format,
        output_limit,
        limits,
        None,
        tx,
    )
    .await
}

/// [`stream_turn`] for a system prompt whose composer reported its
/// `sections`: the request's manifest names them while `messages[0]` is still
/// the text they describe, and drops them, flagged stale, when it is not
/// (a compaction rewrote it). Without `sections` the manifest still carries
/// the hashes of the system text, the tools and the dynamic suffix.
///
/// # Errors
///
/// As [`stream_turn`].
#[allow(clippy::too_many_arguments)]
pub async fn stream_turn_attributed(
    model: &dyn ModelProvider,
    messages: Vec<Message>,
    tools: Vec<ToolSchema>,
    cache_key: Option<String>,
    dynamic_suffix: Option<String>,
    response_format: Option<ResponseFormat>,
    output_limit: impl Into<sven_model::budget::OutputLimit>,
    limits: impl Into<TurnLimits>,
    sections: Option<&sven_model::PromptSections>,
    tx: &mpsc::Sender<AgentEvent>,
) -> anyhow::Result<StreamedTurn> {
    let limits = limits.into();
    let output_limit = output_limit.into();
    let core_tool_count = tools.iter().filter(|s| !s.is_mcp).count();
    let (messages, image_limits) = crate::request_images::fit(model, messages);

    let (thinking_token_cap, thinking_time_cap) = thinking_budget_override()
        .unwrap_or(limits.thinking)
        .resolve();

    let sampling = limits.sampling_account(model)?;
    let manifest = PromptManifest::describe(&messages, sections, &tools, dynamic_suffix.as_deref());
    let req = CompletionRequest {
        messages,
        tools: tools.clone(),
        stream: true,
        system_dynamic_suffix: dynamic_suffix,
        cache_key,
        max_output_tokens_override: output_limit.tokens,
        output_limit_is_room: output_limit.is_room,
        core_tool_count,
        response_format,
        sampling: limits.sampling,
        enable_thinking: limits.enable_thinking,
        // A server that forces the end of the reasoning at this budget makes the
        // model answer; the watchdog below only backs it up.
        thinking_budget: thinking_token_cap,
        tool_choice: limits.tool_choice,
        want_logprobs: limits.want_logprobs,
        top_logprobs: limits.top_logprobs,
        sampler_support: limits.sampler_support,
        manifest,
    };

    let mut ledger = LedgerRecorder::start(&req);
    let mut watch = StallWatch::new(model, &limits.stall, limits.stream_idle);
    let mut stream = watch
        .connect(
            async {
                (model.complete(req).await)
                    .map_err(|e| crate::request_images::note_refusal(e, image_limits))
            },
            tx,
        )
        .await?;

    let mut full_text = String::new();
    // The reasoning of the stretch being streamed, and every stretch before it.
    let mut thinking_buf = String::new();
    let mut reasoning = String::new();
    let mut repetition = RepetitionWatch::default();
    // The arguments of the call being streamed, watched for the same block
    // written over and over (a model that loops there never closes the call).
    let mut args_repetition = RepetitionWatch::with_repeats(TOOL_CALL_REPEATS);
    let mut args_slot: Option<u32> = None;
    let mut args_token_estimate: u32 = 0;
    let mut thinking_token_estimate: u32 = 0;
    // Set on the first ThinkingDelta of an uninterrupted reasoning stretch;
    // cleared on any TextDelta or tool-call delta (forward progress).
    let mut thinking_stall_started: Option<crate::clock::Instant> = None;
    // Each call's id, name and argument text as streamed, by its index.
    let mut slots: BTreeMap<u32, AccumSlot> = BTreeMap::new();
    let mut server_record = None;
    let mut truncated = false;
    let mut sampled = Sampled {
        decoding: Some(limits.recorded_decoding())
            .filter(|d| !d.is_empty())
            .map(Box::new),
        sampling_honoured: sampling.honoured,
        sampling_unhonoured: sampling.unhonoured,
        ..Sampled::default()
    };

    // A watchdog's abort: the reply is not filed, but what was sampled is recorded.
    let aborted: Option<anyhow::Error> = loop {
        let event = match watch.next_event(&mut stream, tx).await? {
            None => break None,
            Some(e) => e?,
        };

        if matches!(
            event,
            ResponseEvent::ThinkingDelta(_)
                | ResponseEvent::TextDelta(_)
                | ResponseEvent::ToolCall { .. }
        ) {
            ledger.note_first_token();
        }
        match event {
            // Provider-side output-token ceiling hit: this is genuinely
            // worth surfacing (was previously silently discarded - the
            // model said "I ran out of room" and nothing recorded it) but
            // must NOT be reported via `AgentEvent::Error`/`UiEvent::Error`.
            // The turn still produces a real (if truncated) answer and
            // completes normally below; `AgentEvent::Error` means something
            // stronger to at least one consumer - ACP's prompt loop treats
            // any `AgentEvent::Error` as a hard failure and aborts the whole
            // response (`crates/acp/src/agent.rs`), which would turn a
            // merely-truncated reply into an outright ACP-level error.
            // tracing::warn! surfaces it (grep-able, alertable) without
            // changing what any UI-facing consumer does with the turn.
            ResponseEvent::MaxTokens => {
                warn!("model hit its max-output-tokens limit; response was truncated");
                truncated = true;
                sampled.truncated = true;
            }
            ResponseEvent::ThinkingDelta(delta) => {
                let stall_started =
                    *thinking_stall_started.get_or_insert_with(crate::clock::Instant::now);
                thinking_token_estimate =
                    thinking_token_estimate.saturating_add(estimate_tokens(&delta));
                thinking_buf.push_str(&delta);
                let looped = repetition.feed(&delta);
                let _ = tx.send(AgentEvent::ThinkingDelta(delta)).await;
                if let Some(times) = looped {
                    break Some(anyhow::Error::new(AbortedError::reasoning_loop(format!(
                        "{REASONING_LOOP_PREFIX} the model is looping in its reasoning (the same stretch occurs {times} \
                         times in the last few thousand characters; ~{thinking_token_estimate} thinking \
                         tokens so far)"
                    ))));
                }

                // Thinking-loop watchdog: whichever cap fires first aborts
                // the turn, ended like a cancel but marked as the watchdog's
                // (see `AbortedError`'s doc comment for why).
                // The request carries the budget, and a server that forces the
                // end of the reasoning there lets the model answer: the turn is
                // aborted only a quarter past it, for a server that does not.
                if let Some(cap) = thinking_token_cap.map(|cap| cap.saturating_add(cap / 4)) {
                    if thinking_token_estimate >= cap {
                        break Some(anyhow::Error::new(AbortedError::by_watchdog(format!(
                            "thinking-token limit exceeded ({thinking_token_estimate} >= {cap} estimated tokens); \
                             raise agent.max_thinking_tokens if the model needs to reason longer"
                        ))));
                    }
                }
                if let Some(cap) = thinking_time_cap {
                    if stall_started.elapsed() >= cap {
                        break Some(anyhow::Error::new(AbortedError::by_watchdog(format!(
                            "thinking timeout exceeded ({}s with no forward progress); \
                             raise agent.thinking_timeout_secs if the model needs longer",
                            cap.as_secs()
                        ))));
                    }
                }
            }
            ResponseEvent::TextDelta(delta) if !delta.is_empty() => {
                thinking_stall_started = None; // forward progress resets the stall clock
                repetition.reset();
                if !thinking_buf.is_empty() {
                    let stretch = std::mem::take(&mut thinking_buf);
                    reasoning.push_str(&stretch);
                    let _ = tx.send(AgentEvent::ThinkingComplete(stretch)).await;
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
                thinking_stall_started = None; // forward progress resets the stall clock
                repetition.reset();
                if args_slot != Some(index) {
                    args_slot = Some(index);
                    args_repetition.reset();
                    args_token_estimate = 0;
                }
                args_token_estimate =
                    args_token_estimate.saturating_add(estimate_tokens(&arguments));
                if let Some(times) = args_repetition.feed(&arguments) {
                    break Some(anyhow::Error::new(AbortedError::tool_call_loop(format!(
                        "{TOOL_CALL_LOOP_PREFIX} the model is repeating itself inside a tool call's arguments (the \
                         same block occurs {times} times in a row; ~{args_token_estimate} argument tokens \
                         so far)"
                    ))));
                }
                // The reasoning before a call is complete when the call
                // begins, as it is when text begins: observers file it with
                // the reply that calls, not after it.
                if !thinking_buf.is_empty() {
                    let stretch = std::mem::take(&mut thinking_buf);
                    reasoning.push_str(&stretch);
                    let _ = tx.send(AgentEvent::ThinkingComplete(stretch)).await;
                }
                slots.entry(index).or_default().feed(&id, &name, &arguments);
            }
            ResponseEvent::Sampled(record) => server_record = Some(record),
            ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                cost_usd,
            } => {
                ledger.note_usage(ReportedUsage {
                    input: input_tokens,
                    output: output_tokens,
                    cache_read: cache_read_tokens,
                    cache_write: cache_write_tokens,
                });
                let _ = tx
                    .send(AgentEvent::TokenUsage {
                        input: input_tokens,
                        output: output_tokens,
                        cache_read: cache_read_tokens,
                        cache_write: cache_write_tokens,
                        cache_read_total: 0,
                        cache_write_total: 0,
                        max_tokens: 0,
                        max_output_tokens: 0,
                        cost_usd,
                        breakdown: None,
                    })
                    .await;
            }
            ResponseEvent::Tokens { ids, logprobs } => {
                sampled.token_ids.extend(ids);
                sampled.logprobs.extend(logprobs);
            }
            ResponseEvent::TopTokens { ids, logprobs } => {
                sampled.top_token_ids.extend(ids);
                sampled.top_logprobs.extend(logprobs);
            }
            ResponseEvent::SamplerSupport(support) => match &mut sampled.sampler_support {
                Some(held) => held.append(&support),
                None => sampled.sampler_support = Some(support),
            },
            ResponseEvent::Served { model, seed } => {
                sampled.model = Some(model);
                sampled.seed = sampling.seed_in_effect(seed);
            }
            ResponseEvent::PromptTokens { ids } => sampled.prompt_token_ids.extend(ids),
            ResponseEvent::Identity(identity) => {
                // The seed the server drew or used beats the one asked for.
                if identity.seed.is_some() {
                    sampled.seed = identity.seed;
                }
                sampling.note_reported_seed(&mut sampled, identity.seed);
                sampled.identity.merge(identity);
            }
            ResponseEvent::Done => break None,
            ResponseEvent::Error(e) => {
                return Err(anyhow::anyhow!("model stream error: {e}"));
            }
            _ => {}
        }
    };
    if !thinking_buf.is_empty() {
        let stretch = std::mem::take(&mut thinking_buf);
        reasoning.push_str(&stretch);
        let _ = tx.send(AgentEvent::ThinkingComplete(stretch)).await;
    }
    if let Some(abort) = aborted {
        // Cut mid-reply: nothing of it is filed, and the record of the
        // request says what was sampled before the cut.
        sampled.truncated = true;
        if !sampled.is_empty() {
            let _ = tx
                .send(AgentEvent::ResponseSampled(Box::new(sampled)))
                .await;
        }
        let _ = tx.send(ledger.into_event()).await;
        return Err(abort);
    }

    if let Some(k) = limits.top_logprobs {
        check_top_logprobs(&sampled, k)?;
    }
    if let Some(n) = limits.sampler_support {
        check_sampler_support(&sampled, n)?;
    }

    // The calls in the order the model wrote them. An id is the server's,
    // made unique where it repeats one: a result is paired with its call by id.
    let mut issued_ids = std::collections::HashSet::new();
    let calls: Vec<SampledCall> = slots
        .into_values()
        .enumerate()
        .map(|(n, slot)| {
            let id = if slot.id.is_empty() {
                format!("call_{n}")
            } else {
                slot.id
            };
            SampledCall {
                id: unique_call_id(&mut issued_ids, &id),
                name: slot.name,
                arguments: slot.args,
            }
        })
        .collect();
    let reply = SampledReply {
        text: full_text,
        reasoning,
        calls,
        sampled: server_record,
        finish: if truncated {
            ReplyFinish::Length
        } else {
            ReplyFinish::Stop
        },
    };
    // Each call as the tool runs it: its parsed arguments, or why it cannot run.
    let mut arg_errors = HashMap::new();
    let mut tool_calls = Vec::with_capacity(reply.calls.len());
    for (i, call) in reply.calls.iter().enumerate() {
        let args = reply.arguments(i).unwrap_or_else(|e| {
            warn!(tool_name = %call.name, tool_call_id = %call.id, error = %e, "tool call cannot run");
            arg_errors.insert(call.id.clone(), e);
            serde_json::Value::Null
        });
        let tc = ToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            args,
        };
        let _ = tx.send(AgentEvent::ToolCallStarted(tc.clone())).await;
        tool_calls.push(tc);
    }

    if !reply.text.is_empty() {
        let _ = tx.send(AgentEvent::TextComplete(reply.text.clone())).await;
    }
    if !sampled.is_empty() {
        let _ = tx
            .send(AgentEvent::ResponseSampled(Box::new(sampled)))
            .await;
    }
    // Last, so a consumer that folds events into turn-shaped steps has the
    // response's content in hand when the request's ledger record arrives.
    let _ = tx.send(ledger.into_event()).await;

    Ok(StreamedTurn {
        reply,
        tool_calls,
        arg_errors,
        truncated,
        thinking_tokens: thinking_token_estimate,
    })
}

#[cfg(test)]
// `OVERRIDE_TEST_LOCK` (below) is deliberately held across the `.await` of
// `stream_turn`: the process-wide thinking-budget override must stay pinned
// for the whole test body, which is exactly what the lint warns about and
// exactly what serialising these tests requires. Scoped to this test module,
// so the lint still applies to production code.
#[allow(clippy::await_holding_lock)]
mod thinking_watchdog_tests {
    use super::*;
    use async_trait::async_trait;
    use sven_model::{CompletionRequest, ResponseEvent};

    /// `THINKING_BUDGET_OVERRIDE` is a process-wide global, and cargo's test
    /// runner executes tests in this module concurrently on separate
    /// threads of the same process. Every test that calls `stream_turn` (and
    /// so is sensitive to whatever the override currently is) or that sets
    /// the override itself must hold this lock for its whole body, or one
    /// test's override could bleed into another's assertions. Resets the
    /// override to `None` on acquisition, so a prior test panicking mid-body
    /// (skipping its own cleanup) can't poison the ones after it either.
    static OVERRIDE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn locked_no_override() -> std::sync::MutexGuard<'static, ()> {
        let guard = OVERRIDE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set_thinking_budget_override(None);
        guard
    }

    /// Streams `ThinkingDelta`s forever - reproduces a model stuck looping in
    /// its own reasoning, never converging to a `TextDelta` or `Done`. Each
    /// delta is `"loop "` repeated `words_per_delta` times, so the caller
    /// controls exactly how many estimated tokens each delta contributes
    /// (`estimate_tokens` is chars/4, and `"loop "` is 5 chars).
    struct EndlessThinkingProvider {
        context_window: Option<u32>,
        words_per_delta: usize,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl ModelProvider for EndlessThinkingProvider {
        fn name(&self) -> &str {
            "endless-thinking"
        }
        fn model_name(&self) -> &str {
            "endless-thinking"
        }
        fn catalog_context_window(&self) -> Option<u32> {
            self.context_window
        }
        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> anyhow::Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>,
        > {
            let delta = "loop ".repeat(self.words_per_delta);
            let stream = futures::stream::repeat_with(move || {
                Ok(ResponseEvent::ThinkingDelta(delta.clone()))
            });
            Ok(Box::pin(stream))
        }
    }

    /// A provider whose single tool call never ends: its arguments are the
    /// same pair of Ex commands written forever.
    struct EndlessToolCallProvider;

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl ModelProvider for EndlessToolCallProvider {
        fn name(&self) -> &str {
            "endless-tool-call"
        }
        fn model_name(&self) -> &str {
            "endless-tool-call"
        }
        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> anyhow::Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>,
        > {
            let mut first = true;
            let stream = futures::stream::repeat_with(move || {
                let (id, name, arguments) = if std::mem::take(&mut first) {
                    ("call_1", "vim-vim", "{\"commands\": [\"args uart.c\"")
                } else {
                    (
                        "",
                        "",
                        ", \"1\", \"normal! A void uart_writeputs(const char *s);\"",
                    )
                };
                Ok(ResponseEvent::ToolCall {
                    index: 0,
                    id: id.into(),
                    name: name.into(),
                    arguments: arguments.into(),
                })
            });
            Ok(Box::pin(stream))
        }
    }

    /// A provider that connects and then never sends a chunk.
    struct SilentProvider;

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl ModelProvider for SilentProvider {
        fn name(&self) -> &str {
            "silent"
        }
        fn model_name(&self) -> &str {
            "silent"
        }
        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> anyhow::Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>,
        > {
            Ok(Box::pin(futures::stream::pending()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_silent_past_the_configured_limit_fails_the_turn() {
        let (tx, drain) = drained_channel();
        let started = tokio::time::Instant::now();
        let result = stream_turn(
            &SilentProvider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            TurnLimits::default().with_stream_idle_secs(Some(7)),
            &tx,
        )
        .await;
        drop(tx);
        drain.await.unwrap();
        let err = result.expect_err("a silent stream is stale");
        assert!(err.to_string().contains("idle for >7 s"), "{err}");
        assert_eq!(started.elapsed(), Duration::from_secs(7));
    }

    /// Answers every request with the same events.
    struct ScriptProvider(Vec<ResponseEvent>);

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
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
            Ok(Box::pin(futures::stream::iter(
                self.0.clone().into_iter().map(Ok),
            )))
        }
    }

    #[tokio::test]
    async fn the_reasoning_before_a_call_completes_before_the_call_is_announced() {
        let provider = ScriptProvider(vec![
            ResponseEvent::ThinkingDelta("ask the tool".into()),
            ResponseEvent::ToolCall {
                index: 0,
                id: "c1".into(),
                name: "weather".into(),
                arguments: "{}".into(),
            },
            ResponseEvent::Done,
        ]);
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        let turn = stream_turn(
            &provider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            TurnLimits::default(),
            &tx,
        )
        .await
        .expect("the turn completes");
        drop(tx);
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        let thought = events
            .iter()
            .position(|e| matches!(e, AgentEvent::ThinkingComplete(_)));
        let called = events
            .iter()
            .position(|e| matches!(e, AgentEvent::ToolCallStarted(_)));
        assert!(thought.is_some() && called.is_some(), "{events:?}");
        assert!(
            thought < called,
            "the reasoning is filed with the reply that calls: {events:?}"
        );
        assert_eq!(turn.reply.reasoning, "ask the tool");
    }

    fn drained_channel() -> (mpsc::Sender<AgentEvent>, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        (tx, drain)
    }

    #[tokio::test]
    async fn token_cap_aborts_a_looping_turn() {
        let _guard = locked_no_override();
        let provider = EndlessThinkingProvider {
            context_window: None,
            words_per_delta: 5, // "loop " x5 = 25 chars = 6 estimated tokens per delta
        };
        let (tx, drain) = drained_channel();

        let result = stream_turn(
            &provider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            ThinkingBudget {
                max_thinking_tokens: Some(1),
                thinking_timeout_secs: Some(9999), // must not be the trigger
            },
            &tx,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        let err = result.expect_err("an endlessly-looping turn must not succeed");
        let aborted = err
            .downcast_ref::<AbortedError>()
            .expect("must be reported as AbortedError, not a generic failure");
        assert!(
            aborted.reason.contains("thinking-token limit"),
            "unexpected message: {}",
            aborted.reason
        );
    }

    #[tokio::test]
    async fn time_cap_aborts_a_looping_turn_even_under_the_token_cap() {
        let _guard = locked_no_override();
        let provider = EndlessThinkingProvider {
            context_window: None,
            words_per_delta: 5,
        };
        let (tx, drain) = drained_channel();

        // thinking_timeout_secs: 0 makes the very first delta's elapsed time
        // (>= 0) already satisfy the cap - deterministic, no real sleeping.
        let result = stream_turn(
            &provider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            ThinkingBudget {
                max_thinking_tokens: Some(u32::MAX), // must not be the trigger
                thinking_timeout_secs: Some(0),
            },
            &tx,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        let err = result.expect_err("an endlessly-looping turn must not succeed");
        let aborted = err
            .downcast_ref::<AbortedError>()
            .expect("must be reported as AbortedError, not a generic failure");
        assert!(
            aborted.reason.contains("thinking timeout"),
            "unexpected message: {}",
            aborted.reason
        );
    }

    #[tokio::test]
    async fn an_endless_reasoning_loop_is_stopped_without_any_token_cap() {
        let _guard = locked_no_override();
        // No cap is configured and the window is tiny; the model only ever
        // repeats itself, which the repetition detector catches.
        let provider = EndlessThinkingProvider {
            context_window: Some(40),
            words_per_delta: 5,
        };
        let (tx, drain) = drained_channel();

        let result = stream_turn(
            &provider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            ThinkingBudget::default(),
            &tx,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        let err = result.expect_err("a loop must be stopped");
        let aborted = err
            .downcast_ref::<AbortedError>()
            .expect("must be AbortedError");
        assert!(aborted.reason.contains("looping"), "{}", aborted.reason);
    }

    #[tokio::test]
    async fn an_endless_tool_call_is_stopped_as_a_tool_call_loop() {
        let _guard = locked_no_override();
        let (tx, drain) = drained_channel();

        let result = stream_turn(
            &EndlessToolCallProvider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            ThinkingBudget::default(),
            &tx,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        let err = result.expect_err("a tool call that never ends must be stopped");
        let aborted = err
            .downcast_ref::<AbortedError>()
            .expect("must be AbortedError");
        assert!(
            aborted.reason.starts_with(TOOL_CALL_LOOP_PREFIX),
            "{}",
            aborted.reason
        );
        assert_eq!(
            aborted.repetition,
            Some(crate::aborted::Repetition::ToolCall)
        );
    }

    #[tokio::test]
    async fn no_context_window_and_no_override_disables_the_token_cap() {
        let _guard = locked_no_override();
        // With no context window known and no explicit override, only the
        // (very long, effectively inert here) default time cap remains -
        // matches the "no gate when unknown" precedent for the input-budget
        // gate. Prove this by running for a few deltas without aborting via
        // the token path, then cutting the test short with a tiny time cap.
        let provider = EndlessThinkingProvider {
            context_window: None,
            words_per_delta: 5,
        };
        let (tx, drain) = drained_channel();

        let result = stream_turn(
            &provider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            ThinkingBudget {
                max_thinking_tokens: None,
                thinking_timeout_secs: Some(0),
            },
            &tx,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        let err = result.expect_err("must still abort - via the time cap, not the token cap");
        let aborted = err
            .downcast_ref::<AbortedError>()
            .expect("must be AbortedError");
        assert!(
            aborted.reason.contains("thinking timeout"),
            "the token cap must be disabled when the context window is unknown: {}",
            aborted.reason
        );
    }

    #[tokio::test]
    async fn forward_progress_resets_the_stall_clock_not_the_token_count() {
        let _guard = locked_no_override();
        // A provider that alternates one ThinkingDelta with one TextDelta,
        // each separated by a short real sleep. This is the one case that
        // actually distinguishes "reset works" from "reset is broken": with
        // reset working, `thinking_stall_started` is set fresh on every
        // single ThinkingDelta (the immediately preceding TextDelta always
        // cleared it), so its `elapsed()` is ~0 every time it's checked -
        // safely under the time cap no matter how many rounds pass. If reset
        // were broken, the clock would keep aging from the very first
        // ThinkingDelta and cross the time cap after a couple of rounds,
        // well before the token cap (which needs several more rounds) has a
        // chance to fire. The token cap, by contrast, is a per-turn ceiling
        // that is deliberately NOT reset by progress - it must still fire.
        //
        // The time cap can only be expressed in whole seconds (a real
        // user-facing setting has no need for sub-second precision), so this
        // necessarily costs a few real seconds of test time - see the math
        // in the `ThinkingBudget` below.
        const ROUND_DELAY: Duration = Duration::from_millis(350);

        struct AlternatingProvider;

        #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
        #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
        impl ModelProvider for AlternatingProvider {
            fn name(&self) -> &str {
                "alternating"
            }
            fn model_name(&self) -> &str {
                "alternating"
            }
            async fn complete(
                &self,
                _req: CompletionRequest,
            ) -> anyhow::Result<
                std::pin::Pin<
                    Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>,
                >,
            > {
                #[derive(Clone, Copy)]
                enum Phase {
                    Thinking,
                    Text,
                }
                let stream = futures::stream::unfold(Phase::Thinking, |phase| async move {
                    tokio::time::sleep(ROUND_DELAY).await;
                    match phase {
                        // 15 chars = 3 estimated tokens per round.
                        Phase::Thinking => Some((
                            Ok(ResponseEvent::ThinkingDelta("loop loop loop ".into())),
                            Phase::Text,
                        )),
                        // Non-empty, since `TextDelta` is only forward
                        // progress when `!delta.is_empty()` (matches the
                        // real loop's own guard - an empty delta is a no-op).
                        Phase::Text => {
                            Some((Ok(ResponseEvent::TextDelta(".".into())), Phase::Thinking))
                        }
                    }
                });
                Ok(Box::pin(stream))
            }
        }

        let (tx, drain) = drained_channel();
        let result = stream_turn(
            &AlternatingProvider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            ThinkingBudget {
                // 3 tokens/round -> needs 4 rounds (3,6,9,12) to cross 10.
                max_thinking_tokens: Some(10),
                // With reset working, elapsed() is ~0 every check, so this
                // is never approached across all 4 rounds (~2.8s). With
                // reset broken, the un-reset clock crosses 1s by round 3
                // (~350ms*5 = 1750ms since round 1's ThinkingDelta) - firing
                // before round 4's token cap ever gets a chance.
                thinking_timeout_secs: Some(1),
            },
            &tx,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        let err = result.expect_err("the token cap must still fire eventually");
        let aborted = err
            .downcast_ref::<AbortedError>()
            .expect("must be AbortedError");
        assert!(
            aborted.reason.contains("thinking-token limit"),
            "progress must reset the stall clock, not the token count: {}",
            aborted.reason
        );
    }

    #[test]
    fn override_round_trips_and_defaults_to_none() {
        let _guard = locked_no_override();
        assert_eq!(thinking_budget_override(), None, "starts cleared");

        let budget = ThinkingBudget {
            max_thinking_tokens: Some(123),
            thinking_timeout_secs: Some(45),
        };
        set_thinking_budget_override(Some(budget));
        assert_eq!(thinking_budget_override(), Some(budget));

        set_thinking_budget_override(None);
        assert_eq!(thinking_budget_override(), None, "clears back to None");
    }

    #[tokio::test]
    async fn override_takes_priority_over_the_per_call_budget() {
        let _guard = locked_no_override();
        // The per-call budget alone (large caps) would never fire; the
        // override (a tiny cap) must be what actually triggers the abort -
        // proving `/think-limit`'s live override is consulted at all.
        set_thinking_budget_override(Some(ThinkingBudget {
            max_thinking_tokens: Some(1),
            thinking_timeout_secs: Some(9999),
        }));

        let provider = EndlessThinkingProvider {
            context_window: None,
            words_per_delta: 5,
        };
        let (tx, drain) = drained_channel();
        let result = stream_turn(
            &provider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            ThinkingBudget {
                max_thinking_tokens: Some(u32::MAX),
                thinking_timeout_secs: Some(9999),
            },
            &tx,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        let err = result.expect_err("the override's tiny cap must fire");
        let aborted = err
            .downcast_ref::<AbortedError>()
            .expect("must be AbortedError");
        assert!(aborted.reason.contains("thinking-token limit"));
    }

    #[tokio::test]
    async fn clearing_the_override_reverts_to_the_per_call_budget() {
        let _guard = locked_no_override();
        // Set then immediately clear - the tiny cap must NOT apply, and the
        // (never-reached-in-this-short-run) per-call budget takes over.
        set_thinking_budget_override(Some(ThinkingBudget {
            max_thinking_tokens: Some(1),
            thinking_timeout_secs: Some(9999),
        }));
        set_thinking_budget_override(None);

        let provider = EndlessThinkingProvider {
            context_window: None,
            words_per_delta: 5,
        };
        let (tx, drain) = drained_channel();
        // A handful of deltas, then cancel via a tiny time cap on the
        // *per-call* budget - if the (cleared) override were still active,
        // this would instead fail on the 1-token cap on essentially the
        // first delta; asserting the message distinguishes the two.
        let result = stream_turn(
            &provider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            ThinkingBudget {
                max_thinking_tokens: Some(u32::MAX),
                thinking_timeout_secs: Some(0),
            },
            &tx,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        let err = result.expect_err("must still abort, via the per-call time cap");
        let aborted = err
            .downcast_ref::<AbortedError>()
            .expect("must be AbortedError");
        assert!(
            aborted.reason.contains("thinking timeout"),
            "a cleared override must not leave the tiny token cap active: {}",
            aborted.reason
        );
    }
}

#[cfg(test)]
mod max_tokens_tests {
    use super::*;
    use async_trait::async_trait;
    use sven_model::{CompletionRequest, ResponseEvent};

    /// A provider that reports hitting its output-token ceiling mid-stream,
    /// then keeps going to `Done` with a real (if truncated) answer -
    /// exactly what a real provider does. `MaxTokens` must not be treated as
    /// a stream error: the turn should still complete normally.
    struct TruncatedProvider;

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl ModelProvider for TruncatedProvider {
        fn name(&self) -> &str {
            "truncated"
        }
        fn model_name(&self) -> &str {
            "truncated"
        }
        async fn complete(
            &self,
            _req: CompletionRequest,
        ) -> anyhow::Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>,
        > {
            let events: Vec<anyhow::Result<ResponseEvent>> = vec![
                Ok(ResponseEvent::TextDelta("partial ans".into())),
                Ok(ResponseEvent::MaxTokens),
                Ok(ResponseEvent::Done),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn max_tokens_does_not_abort_the_turn() {
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
        let drain = tokio::spawn(async move {
            let mut cut_off = None;
            while let Some(event) = rx.recv().await {
                if let AgentEvent::ResponseSampled(sampled) = event {
                    cut_off = Some(sampled.truncated);
                }
            }
            cut_off
        });

        let result = stream_turn(
            &TruncatedProvider,
            vec![],
            vec![],
            None,
            None,
            None,
            None,
            ThinkingBudget::default(),
            &tx,
        )
        .await;
        drop(tx);
        let cut_off = drain.await.unwrap();

        let turn = result.expect("MaxTokens must not fail the turn");
        assert_eq!(turn.reply.text, "partial ans");
        assert_eq!(
            cut_off,
            Some(true),
            "the record of the call says the response was cut off"
        );
        assert!(turn.tool_calls.is_empty());
    }
}

#[cfg(test)]
mod output_cap_tests {
    use super::TurnLimits;

    #[test]
    fn a_turn_limit_lowers_the_model_output_cap_but_never_raises_it() {
        let capped = TurnLimits::default().with_max_output_tokens(Some(1_000));
        assert_eq!(capped.cap_output(Some(8_192)), Some(1_000));
        assert_eq!(capped.cap_output(Some(500)), Some(500));
        assert_eq!(capped.cap_output(None), Some(1_000));
        assert_eq!(TurnLimits::default().cap_output(Some(8_192)), Some(8_192));
    }
}

#[cfg(test)]
mod default_thinking_cap_tests {
    use super::*;

    #[test]
    fn there_is_no_default_thinking_token_cap() {
        assert_eq!(ThinkingBudget::default().resolve().0, None);
    }

    /// A wall-clock cap measures the server's throughput, not a loop: at 15
    /// tokens/s per stream 600 s ended honest 9k-token reasoning mid-thought.
    /// Loops are caught by repetition and bounded by the output budget.
    #[test]
    fn there_is_no_default_thinking_time_cap() {
        assert_eq!(ThinkingBudget::default().resolve().1, None);
        let explicit = ThinkingBudget {
            thinking_timeout_secs: Some(30),
            ..ThinkingBudget::default()
        };
        assert_eq!(explicit.resolve().1, Some(Duration::from_secs(30)));
    }

    #[test]
    fn an_explicit_cap_is_an_opt_in_that_is_honoured() {
        let explicit = ThinkingBudget {
            max_thinking_tokens: Some(100),
            thinking_timeout_secs: Some(5),
        };
        assert_eq!(
            explicit.resolve(),
            (Some(100), Some(Duration::from_secs(5)))
        );
    }
}
