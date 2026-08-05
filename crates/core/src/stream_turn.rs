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

use sven_model::{CompletionRequest, Message, ModelProvider, ResponseEvent, ResponseFormat, ToolSchema};
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

use crate::events::AgentEvent;
use crate::tool_slots::attempt_json_repair;

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
/// # Errors
///
/// Returns an error if the model API call fails or the stream stalls.
pub async fn stream_turn(
    model: &dyn ModelProvider,
    messages: Vec<Message>,
    tools: Vec<ToolSchema>,
    cache_key: Option<String>,
    dynamic_suffix: Option<String>,
    response_format: Option<ResponseFormat>,
    tx: &mpsc::Sender<AgentEvent>,
) -> anyhow::Result<(String, Vec<ToolCall>)> {
    let core_tool_count = tools.iter().filter(|s| !s.is_mcp).count();
    let modalities = model.input_modalities();
    let messages = sven_model::sanitize::strip_images_if_unsupported(messages, &modalities);

    let req = CompletionRequest {
        messages,
        tools: tools.clone(),
        stream: true,
        system_dynamic_suffix: dynamic_suffix,
        cache_key,
        max_output_tokens_override: None,
        core_tool_count,
        response_format,
    };

    let mut stream = model.complete(req).await.context("model completion failed")?;

    let mut full_text = String::new();
    let mut thinking_buf = String::new();
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
            ResponseEvent::MaxTokens => {}
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
                warn!("model stream error: {e}");
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

    // Mutating a process-global env var races other tests only if they touch
    // the same name; STREAM_CHUNK_TIMEOUT_ENV is dedicated to this test module,
    // so no serialization guard is needed beyond restoring the original value.
    #[test]
    fn defaults_to_300s_when_unset() {
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
        let orig = std::env::var_os(STREAM_CHUNK_TIMEOUT_ENV);
        for bad in ["0", "-5", "not-a-number", ""] {
            unsafe { std::env::set_var(STREAM_CHUNK_TIMEOUT_ENV, bad) };
            assert_eq!(stream_chunk_timeout(), DEFAULT_STREAM_CHUNK_TIMEOUT, "input {bad:?} should fall back to default");
        }
        unsafe {
            match &orig {
                Some(v) => std::env::set_var(STREAM_CHUNK_TIMEOUT_ENV, v),
                None => std::env::remove_var(STREAM_CHUNK_TIMEOUT_ENV),
            }
        }
    }
}
