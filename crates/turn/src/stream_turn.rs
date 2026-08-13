// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Single-turn streaming primitive: stream one model response and collect
//! proposed tool calls **without executing them**.
//!
//! [`stream_turn`] is the reusable kernel-friendly building block that
//! [`TurnExecutor`] calls.  It is accumulation-only: JSON arguments are
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

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context as _;
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::warn;

use sven_model::{
    CompletionRequest, Message, ModelProvider, ResponseEvent, ResponseFormat, ToolSchema,
};
use sven_tools::ToolCall;

/// Convert tool-registry schemas into model-API schemas, preserving the order
/// produced by [`sven_tools::ToolRegistry::schemas_for_names`].
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

use crate::tool_slots::attempt_json_repair;
use sven_vocab::SessionEvent as AgentEvent;

/// Marker error distinguishing a deliberate abort - a user cancel (Esc,
/// Ctrl+C, `/abort`, ACP `cancel()`) or the thinking-token/time watchdog -
/// from a genuine turn failure. `TurnExecutor` wraps its own cancel-channel
/// trigger in this, and [`stream_turn`]'s watchdog check does the same, so a
/// single `downcast_ref::<AbortedError>()` after the turn's `tokio::select!`
/// collapses both onto `Event::UserCancelled`/`UiEvent::Aborted` instead of
/// `Event::LlmFailed`/`UiEvent::Error` - the latter drives `SdlcMachine` into
/// `Recovery`/`Failed`, which is correct for a real failure but wrong for a
/// turn the user (or the watchdog) deliberately cut short.
#[derive(Debug)]
pub struct AbortedError(pub String);

impl std::fmt::Display for AbortedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for AbortedError {}

/// Callback that resolves a model string (e.g. `"anthropic/claude-opus"`) to a
/// live [`ModelProvider`].  Provided by the bootstrap layer so that `sven-core`
/// can switch models mid-turn without depending on the full
/// `sven-config::Config`.
pub type ModelResolver =
    std::sync::Arc<dyn Fn(&str) -> anyhow::Result<std::sync::Arc<dyn ModelProvider>> + Send + Sync>;

/// Default maximum idle time between stream chunks before the connection is
/// declared stale, when `SVEN_STREAM_CHUNK_TIMEOUT_SECS` is unset.
const DEFAULT_STREAM_CHUNK_TIMEOUT: Duration = Duration::from_secs(300);

/// Environment variable overriding [`DEFAULT_STREAM_CHUNK_TIMEOUT`]. A slow but
/// live provider -- e.g. CPU-backed prefill of a large tool-schema-laden prompt,
/// which can legitimately stay silent on the wire well past 300s before its
/// first streamed token -- would otherwise be indistinguishable from a genuinely
/// stale connection and abort the turn.
const STREAM_CHUNK_TIMEOUT_ENV: &str = "SVEN_STREAM_CHUNK_TIMEOUT_SECS";

/// Resolve the per-chunk stream idle timeout: [`STREAM_CHUNK_TIMEOUT_ENV`] if
/// set to a valid positive integer, else [`DEFAULT_STREAM_CHUNK_TIMEOUT`].
fn stream_chunk_timeout() -> Duration {
    std::env::var(STREAM_CHUNK_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&secs| secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_STREAM_CHUNK_TIMEOUT)
}

// ─── Thinking-loop watchdog ─────────────────────────────────────────────────

/// Fraction of the model's context window used as the default thinking-token
/// cap when no explicit override is configured (see [`ThinkingBudget`]).
const DEFAULT_THINKING_TOKEN_FRACTION: f64 = 0.10;

/// Default time a model may spend emitting `ThinkingDelta`s with no forward
/// progress (a `TextDelta` or a tool-call delta) before the turn is aborted.
const DEFAULT_THINKING_TIMEOUT: Duration = Duration::from_secs(600);

/// Configured caps for the thinking-loop watchdog: guards against a model
/// (observed with some local reasoning models, e.g. Qwen) that loops
/// indefinitely in its reasoning instead of converging to an answer. Two
/// independent triggers, whichever fires first:
///
/// - **Token cap**: total estimated `ThinkingDelta` tokens (chars/4, matching
///   the TUI's own streaming estimate) for this one `stream_turn` call. Not
///   reset by forward progress - it is a resource ceiling for the whole turn.
/// - **Time cap**: wall-clock time spent emitting `ThinkingDelta`s *without*
///   forward progress. Reset on every `TextDelta` or tool-call delta, so a
///   legitimately long multi-step turn is never killed - only a stalled
///   reasoning loop is.
///
/// `None` fields fall back to the defaults on [`ThinkingBudget::resolve`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThinkingBudget {
    /// Cap on estimated thinking tokens. `None` defaults to
    /// [`DEFAULT_THINKING_TOKEN_FRACTION`] of the model's resolved context
    /// window (via `model.catalog_context_window()`) - or no cap at all when
    /// the window isn't known, matching the "no gate when unknown" precedent
    /// in `sven_model::budget::effective_input_budget`.
    pub max_thinking_tokens: Option<u32>,
    /// Cap on time spent thinking without forward progress. `None` defaults
    /// to [`DEFAULT_THINKING_TIMEOUT`].
    pub thinking_timeout_secs: Option<u64>,
}

impl ThinkingBudget {
    /// Build from the workspace-wide `agent.max_thinking_tokens` /
    /// `agent.thinking_timeout_secs` config fields (`sven_config::AgentConfig`).
    /// A `None` field there means "use the built-in default" - preserved
    /// as-is here rather than eagerly resolved, since the default token cap
    /// depends on the model in use (via `context_window`, only known once a
    /// specific `stream_turn` call has a `model` reference).
    #[must_use]
    pub fn from_agent_config(cfg: &sven_config::AgentConfig) -> Self {
        Self {
            max_thinking_tokens: cfg.max_thinking_tokens,
            thinking_timeout_secs: cfg.thinking_timeout_secs,
        }
    }

    /// Resolve the effective `(token_cap, time_cap)`, filling in defaults for
    /// unset fields. `context_window` is only consulted for the default
    /// token cap; an explicit `max_thinking_tokens` always wins.
    fn resolve(self, context_window: Option<u32>) -> (Option<u32>, Duration) {
        let tokens = self.max_thinking_tokens.or_else(|| {
            context_window
                .filter(|&w| w > 0)
                .map(|w| (f64::from(w) * DEFAULT_THINKING_TOKEN_FRACTION) as u32)
        });
        let timeout = self
            .thinking_timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_THINKING_TIMEOUT);
        (tokens, timeout)
    }
}

/// Rough token estimate matching the TUI's own live streaming counter
/// (`crates/tui/src/app/agent_events.rs`) - chars/4, not a real tokenizer.
/// Good enough for a loop-prevention ceiling; not used for billing.
fn estimate_tokens(text: &str) -> u32 {
    (text.chars().count() / 4) as u32
}

/// Process-wide live override for the thinking-loop watchdog, set by the
/// `/think-limit` slash command so the caps can be adjusted without
/// restarting sven. Takes priority over the `ThinkingBudget` each caller
/// passes to [`stream_turn`] (which itself comes from config), starting with
/// the very next `stream_turn` call anywhere in the process - including every
/// concurrent session/subagent, which is the intended scope for a "the model
/// is looping, turn this down right now" escape hatch.
static THINKING_BUDGET_OVERRIDE: std::sync::OnceLock<std::sync::Mutex<Option<ThinkingBudget>>> =
    std::sync::OnceLock::new();

/// Set (`Some`) or clear (`None`, reverting to config) the process-wide
/// thinking-budget override. See [`THINKING_BUDGET_OVERRIDE`].
pub fn set_thinking_budget_override(budget: Option<ThinkingBudget>) {
    let cell = THINKING_BUDGET_OVERRIDE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut guard) = cell.lock() {
        *guard = budget;
    }
}

/// Read the current process-wide override, if any - used by `/think-limit`
/// with no arguments to report what's currently active.
#[must_use]
pub fn thinking_budget_override() -> Option<ThinkingBudget> {
    THINKING_BUDGET_OVERRIDE
        .get()
        .and_then(|cell| cell.lock().ok().and_then(|guard| *guard))
}

// ─── Accumulation-only slot ───────────────────────────────────────────────────

struct AccumSlot {
    id: String,
    name: String,
    args_buf: String,
}

impl AccumSlot {
    fn feed(&mut self, id: &str, name: &str, args_chunk: &str) -> Option<ToolCall> {
        if !id.is_empty() {
            self.id = id.to_owned();
        }
        if !name.is_empty() {
            self.name = name.to_owned();
        }
        self.args_buf.push_str(args_chunk);

        if self.args_buf.ends_with('}') {
            if let Some(tc) = self.try_finalize() {
                return Some(tc);
            }
        }
        None
    }

    fn try_finalize(&self) -> Option<ToolCall> {
        let args: serde_json::Value = serde_json::from_str(&self.args_buf).ok()?;
        Some(ToolCall {
            id: self.id.clone(),
            name: self.name.clone(),
            args,
        })
    }

    fn finalize(self) -> ToolCall {
        let args = if self.args_buf.is_empty() {
            warn!(
                tool_name = %self.name,
                tool_call_id = %self.id,
                "model sent tool call with empty arguments; substituting {{}}"
            );
            serde_json::Value::Object(Default::default())
        } else {
            serde_json::from_str(&self.args_buf)
                .or_else(|_| attempt_json_repair(&self.args_buf))
                .unwrap_or_else(|_| {
                    warn!(
                        tool_name = %self.name,
                        tool_call_id = %self.id,
                        args_buf = %self.args_buf,
                        "model sent tool call with invalid JSON; substituting {{}}"
                    );
                    serde_json::Value::Object(Default::default())
                })
        };
        ToolCall {
            id: self.id,
            name: self.name,
            args,
        }
    }
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Stream a single model turn and collect all proposed tool calls (accumulation-only).
///
/// Returns `(text, tool_calls)`:
/// - `text`: complete assistant text (empty when the model made only tool calls)
/// - `tool_calls`: all tool calls proposed during this turn (JSON-accumulated,
///   **not executed**)
///
/// `tx` receives [`AgentEvent`]s for UI bridging while the stream runs.
///
/// `max_output_tokens_override`, when set, is forwarded verbatim as the
/// request's output-token limit (see [`sven_model::budget::dynamic_output_budget`]
/// for how callers typically compute it — scaled to the actual prompt size
/// rather than a fixed reservation). `None` lets the provider apply its own
/// built-in default, unchanged from before this parameter existed.
///
/// `thinking_budget` bounds a runaway reasoning loop - see [`ThinkingBudget`].
/// When either cap is exceeded this returns `Err` wrapping an
/// [`AbortedError`], which callers must route through the same
/// `UserCancelled`/`Aborted` handling as a user-initiated cancel, not a
/// genuine failure.
///
/// # Errors
///
/// Returns an error if the model API call fails, the stream stalls, or the
/// thinking budget is exceeded.
#[allow(clippy::too_many_arguments)]
pub async fn stream_turn(
    model: &dyn ModelProvider,
    messages: Vec<Message>,
    tools: Vec<ToolSchema>,
    cache_key: Option<String>,
    dynamic_suffix: Option<String>,
    response_format: Option<ResponseFormat>,
    max_output_tokens_override: Option<u32>,
    thinking_budget: ThinkingBudget,
    tx: &mpsc::Sender<AgentEvent>,
) -> anyhow::Result<(String, Vec<ToolCall>)> {
    let core_tool_count = tools.iter().filter(|s| !s.is_mcp).count();
    let modalities = model.input_modalities();
    let messages = sven_model::sanitize::strip_images_if_unsupported(messages, &modalities);

    let (thinking_token_cap, thinking_time_cap) = thinking_budget_override()
        .unwrap_or(thinking_budget)
        .resolve(model.catalog_context_window());

    let req = CompletionRequest {
        messages,
        tools: tools.clone(),
        stream: true,
        system_dynamic_suffix: dynamic_suffix,
        cache_key,
        max_output_tokens_override,
        core_tool_count,
        response_format,
    };

    let mut stream = model
        .complete(req)
        .await
        .context("model completion failed")?;

    let mut full_text = String::new();
    let mut thinking_buf = String::new();
    let mut thinking_token_estimate: u32 = 0;
    // Set on the first ThinkingDelta of an uninterrupted reasoning stretch;
    // cleared on any TextDelta or tool-call delta (forward progress).
    let mut thinking_stall_started: Option<std::time::Instant> = None;
    let mut slots: HashMap<u32, AccumSlot> = HashMap::new();
    let mut completed: Vec<ToolCall> = Vec::new();
    let mut completed_indices: Vec<u32> = Vec::new();
    let chunk_timeout = stream_chunk_timeout();

    loop {
        let maybe_event = tokio::time::timeout(chunk_timeout, stream.next())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "model stream idle for >{} s - stale connection",
                    chunk_timeout.as_secs()
                )
            })?;

        let event = match maybe_event {
            None => break,
            Some(e) => e?,
        };

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
            }
            ResponseEvent::ThinkingDelta(delta) => {
                let stall_started =
                    *thinking_stall_started.get_or_insert_with(std::time::Instant::now);
                thinking_token_estimate =
                    thinking_token_estimate.saturating_add(estimate_tokens(&delta));
                thinking_buf.push_str(&delta);
                let _ = tx.send(AgentEvent::ThinkingDelta(delta)).await;

                // Thinking-loop watchdog: whichever cap fires first aborts
                // the turn through the same path as a user-initiated cancel
                // (see `AbortedError`'s doc comment for why).
                if let Some(cap) = thinking_token_cap {
                    if thinking_token_estimate >= cap {
                        return Err(anyhow::Error::new(AbortedError(format!(
                            "thinking-token limit exceeded ({thinking_token_estimate} >= {cap} estimated tokens)"
                        ))));
                    }
                }
                if stall_started.elapsed() >= thinking_time_cap {
                    return Err(anyhow::Error::new(AbortedError(format!(
                        "thinking timeout exceeded ({}s with no forward progress)",
                        thinking_time_cap.as_secs()
                    ))));
                }
            }
            ResponseEvent::TextDelta(delta) if !delta.is_empty() => {
                thinking_stall_started = None; // forward progress resets the stall clock
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
                thinking_stall_started = None; // forward progress resets the stall clock
                let slot = slots.entry(index).or_insert_with(|| AccumSlot {
                    id: String::new(),
                    name: String::new(),
                    args_buf: String::new(),
                });
                if let Some(tc) = slot.feed(&id, &name, &arguments) {
                    if !tc.name.is_empty() {
                        let _ = tx.send(AgentEvent::ToolCallStarted(tc.clone())).await;
                        completed_indices.push(index);
                        completed.push(tc);
                    }
                }
            }
            ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                cost_usd,
            } => {
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
                    })
                    .await;
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
                return Err(anyhow::anyhow!("model stream error: {e}"));
            }
            _ => {}
        }
    }

    // Reclassify inline <think> blocks emitted by models that don't use
    // reasoning_content (e.g. local GGUF without reasoning_format: deepseek).
    if !full_text.is_empty() && thinking_buf.is_empty() {
        if let Some(inline_think) = extract_inline_think_block(&full_text) {
            let _ = tx.send(AgentEvent::ThinkingComplete(inline_think)).await;
            full_text.clear();
        }
    }

    // Finalise any slots whose JSON args were still incomplete when Done arrived.
    let slot_count = slots.len();
    for (idx, slot) in slots {
        if completed_indices.contains(&idx) {
            continue;
        }
        if slot.name.is_empty() {
            warn!(
                tool_call_id = %slot.id,
                "dropping tool call with empty name; cannot dispatch"
            );
            continue;
        }
        let mut tc = slot.finalize();
        if tc.id.is_empty() {
            tc.id = format!("tc_synthetic_{slot_count}");
            warn!(
                tool_name = %tc.name,
                tool_call_id = %tc.id,
                "tool call had empty id; generated synthetic id"
            );
        }
        let _ = tx.send(AgentEvent::ToolCallStarted(tc.clone())).await;
        completed.push(tc);
    }

    // Anthropic-style <invoke> fallback for models that don't emit native tool
    // calls and instead embed function-call markup in the text.
    if completed.is_empty() && full_text.contains("<invoke ") {
        let (cleaned, invoke_calls) = extract_inline_invoke_tool_calls(&full_text);
        if !invoke_calls.is_empty() {
            warn!(
                count = invoke_calls.len(),
                "model emitted Anthropic-style <invoke> tool calls in text; extracting"
            );
            full_text = cleaned;
            for tc in invoke_calls {
                let _ = tx.send(AgentEvent::ToolCallStarted(tc.clone())).await;
                completed.push(tc);
            }
        }
    }

    if !full_text.is_empty() {
        let _ = tx.send(AgentEvent::TextComplete(full_text.clone())).await;
    }

    Ok((full_text, completed))
}

/// Strip `<think>` / `</think>` wrapper tags from accumulated thinking content.
///
/// Some model servers (llama.cpp without `reasoning_format: deepseek`,
/// certain OpenAI-compat proxies) forget to strip these tags before placing
/// the text in `reasoning_content`.  The result is that the thinking buffer
/// contains the raw markup, e.g. `<think>\nStep 1: ...\n</think>`, instead of
/// the clean inner text.  Stripping them here keeps the thinking log readable
/// and prevents the `<think>` noise from leaking into conversation history.
pub(crate) fn strip_think_wrappers(s: String) -> String {
    let trimmed = s.trim();
    let inner = trimmed.strip_prefix("<think>").unwrap_or(trimmed);
    let inner = inner.strip_suffix("</think>").unwrap_or(inner);
    inner.trim().to_string()
}

/// Detect a `<think>...</think>` block occupying the *entire* text.
///
/// Some models emit thinking as plain text deltas (no `reasoning_content`)
/// when the serving layer isn't configured for reasoning extraction.  If the
/// whole text response is a `<think>` block - with or without a closing tag
/// (the model may have been cut off) - the "response" carries no useful
/// content.  Return the extracted inner text so the caller can reclassify
/// it as thinking and clear `full_text`, which causes the turn to be treated
/// as thinking-only and apply the empty-turn retry nudge.
///
/// Returns `None` when the text contains content outside the `<think>` block.
pub(crate) fn extract_inline_think_block(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // Must start with <think>
    let inner = trimmed.strip_prefix("<think>")?;
    // Strip an optional closing tag; an unclosed block (model truncated) is
    // still all-thinking if there is nothing after the last </think>.
    let inner = inner.strip_suffix("</think>").unwrap_or(inner);
    // Reject if there's a *second* </think> inside, which would mean there's
    // real content after the first block.
    if inner.contains("</think>") {
        return None;
    }
    Some(inner.trim().to_string())
}

/// Extract Anthropic-style `<invoke>` tool calls written inline in the text
/// stream by models (e.g. MiniMax) that fall back to the old XML
/// function-call format instead of using the structured tool-call protocol.
///
/// Format:
/// ```text
/// <invoke name="tool_name">
/// <parameter name="param1">value1</parameter>
/// <parameter name="param2">value2</parameter>
/// </invoke>
/// ```
///
/// Returns the text with all `<invoke>...</invoke>` blocks removed and the
/// extracted [`ToolCall`] objects.  Parameter values that parse as valid JSON
/// are stored as JSON; otherwise they are stored as plain strings.
pub(crate) fn extract_inline_invoke_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    use regex::Regex;

    let invoke_re = Regex::new(r#"(?s)<invoke\s+name="([^"]+)">(.*?)</invoke>"#).unwrap();
    let param_re = Regex::new(r#"(?s)<parameter\s+name="([^"]+)">(.*?)</parameter>"#).unwrap();

    let mut tool_calls = Vec::new();

    for cap in invoke_re.captures_iter(text) {
        let name = cap[1].to_string();
        let body = &cap[2];

        let mut args = serde_json::Map::new();
        for param in param_re.captures_iter(body) {
            let key = param[1].to_string();
            let raw = param[2].trim().to_string();
            // Try to decode as JSON (for nested objects/arrays); fall back to
            // a plain string value.
            let val = serde_json::from_str::<serde_json::Value>(&raw)
                .unwrap_or(serde_json::Value::String(raw));
            args.insert(key, val);
        }

        tool_calls.push(ToolCall {
            id: format!("invoke_{}", uuid::Uuid::new_v4().simple()),
            name,
            args: serde_json::Value::Object(args),
        });
    }

    let cleaned = invoke_re.replace_all(text, "").trim().to_string();
    (cleaned, tool_calls)
}

#[cfg(test)]
mod stream_chunk_timeout_tests {
    use super::*;

    // Mutating a process-global env var races every other test in this
    // module that touches the same name - cargo runs `#[test]`s in this
    // binary concurrently on separate threads, and restoring the original
    // value at the end of each test does not prevent one test's temporary
    // value from being visible to another test's `stream_chunk_timeout()`
    // call while both are mid-flight. (Previously unguarded - a genuine,
    // reproducible flake: `falls_back_to_default_on_zero_or_garbage` could
    // observe `honors_a_valid_override`'s "900" instead of its own "0".)
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn locked_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn defaults_to_300s_when_unset() {
        let _guard = locked_env();
        let orig = std::env::var_os(STREAM_CHUNK_TIMEOUT_ENV);
        unsafe { std::env::remove_var(STREAM_CHUNK_TIMEOUT_ENV) };
        assert_eq!(stream_chunk_timeout(), DEFAULT_STREAM_CHUNK_TIMEOUT);
        unsafe {
            match &orig {
                Some(v) => std::env::set_var(STREAM_CHUNK_TIMEOUT_ENV, v),
                None => std::env::remove_var(STREAM_CHUNK_TIMEOUT_ENV),
            }
        }
    }

    #[test]
    fn honors_a_valid_override() {
        let _guard = locked_env();
        let orig = std::env::var_os(STREAM_CHUNK_TIMEOUT_ENV);
        unsafe { std::env::set_var(STREAM_CHUNK_TIMEOUT_ENV, "900") };
        assert_eq!(stream_chunk_timeout(), Duration::from_secs(900));
        unsafe {
            match &orig {
                Some(v) => std::env::set_var(STREAM_CHUNK_TIMEOUT_ENV, v),
                None => std::env::remove_var(STREAM_CHUNK_TIMEOUT_ENV),
            }
        }
    }

    #[test]
    fn falls_back_to_default_on_zero_or_garbage() {
        let _guard = locked_env();
        let orig = std::env::var_os(STREAM_CHUNK_TIMEOUT_ENV);
        for bad in ["0", "-5", "not-a-number", ""] {
            unsafe { std::env::set_var(STREAM_CHUNK_TIMEOUT_ENV, bad) };
            assert_eq!(
                stream_chunk_timeout(),
                DEFAULT_STREAM_CHUNK_TIMEOUT,
                "input {bad:?} should fall back to default"
            );
        }
        unsafe {
            match &orig {
                Some(v) => std::env::set_var(STREAM_CHUNK_TIMEOUT_ENV, v),
                None => std::env::remove_var(STREAM_CHUNK_TIMEOUT_ENV),
            }
        }
    }
}

#[cfg(test)]
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

    #[async_trait]
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
            aborted.0.contains("thinking-token limit"),
            "unexpected message: {}",
            aborted.0
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
            aborted.0.contains("thinking timeout"),
            "unexpected message: {}",
            aborted.0
        );
    }

    #[tokio::test]
    async fn default_token_cap_is_ten_percent_of_context_window() {
        let _guard = locked_no_override();
        // context_window=40, default fraction 10% -> cap=4 estimated tokens.
        // words_per_delta=5 -> 6 estimated tokens per delta, so the very
        // first delta already exceeds the cap.
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

        let err = result.expect_err("the default 10% cap must still apply");
        let aborted = err
            .downcast_ref::<AbortedError>()
            .expect("must be AbortedError");
        assert!(aborted.0.contains("thinking-token limit"));
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
            aborted.0.contains("thinking timeout"),
            "the token cap must be disabled when the context window is unknown: {}",
            aborted.0
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

        #[async_trait]
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
            aborted.0.contains("thinking-token limit"),
            "progress must reset the stall clock, not the token count: {}",
            aborted.0
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
        assert!(aborted.0.contains("thinking-token limit"));
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
            aborted.0.contains("thinking timeout"),
            "a cleared override must not leave the tiny token cap active: {}",
            aborted.0
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

    #[async_trait]
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
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

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
        let _ = drain.await;

        let (text, tool_calls) = result.expect("MaxTokens must not fail the turn");
        assert_eq!(text, "partial ans");
        assert!(tool_calls.is_empty());
    }
}
