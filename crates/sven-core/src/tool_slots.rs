// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Tool-call slot accumulator for LLM streaming.
//!
//! [`ToolSlotManager`] accumulates per-slot JSON argument chunks from the LLM
//! stream without executing any tools. Once the stream ends, callers use
//! [`ToolSlotManager::drain_calls`] to collect the proposed [`ToolCall`]s.
//!
//! Tool execution is the sole responsibility of the HSM kernel (`ToolExecutor`);
//! this module only handles JSON accumulation and repair.
//!
//! ## Session ordering invariant
//!
//! OpenAI's API requires all assistant `ToolCall` messages to appear before
//! any `ToolResult` messages in a single turn.  This constraint is preserved:
//! [`ToolSlotManager::drain_calls`] returns calls sorted by slot index.

use std::collections::HashMap;
use std::sync::Arc;

use tracing::warn;

use sven_tools::{ToolCall, ToolRegistry};

// ─── PendingSlot ─────────────────────────────────────────────────────────────

/// Accumulates streaming argument chunks for a single tool-call slot.
struct PendingSlot {
    id: String,
    name: String,
    /// Raw JSON argument bytes accumulated across stream deltas.
    args_buf: String,
}

impl PendingSlot {
    /// Apply chunk fields and return `Some(ToolCall)` when the slot is ready
    /// to dispatch (args form a valid JSON object or can be repaired).
    ///
    /// Returns `None` when the args are still incomplete.
    fn feed(&mut self, id: &str, name: &str, args_chunk: &str) -> Option<ToolCall> {
        if !id.is_empty() {
            self.id = id.to_owned();
        }
        if !name.is_empty() {
            self.name = name.to_owned();
        }
        self.args_buf.push_str(args_chunk);

        // Probe-parse: attempt to parse accumulated args as JSON. This is
        // cheap (< 1 µs for typical tool args) and lets us dispatch the
        // moment the LLM emits a complete argument object, rather than after
        // the stream ends.  Invalid/partial JSON is the common case while
        // streaming, so parse failures are expected and non-fatal here.
        if self.args_buf.ends_with('}') {
            if let Some(tc) = self.try_finalize() {
                return Some(tc);
            }
        }
        None
    }

    /// Force-finalize using the repair path. Called after stream `Done`.
    fn finalize(self) -> ToolCall {
        let args = if self.args_buf.is_empty() {
            warn!(
                tool_name = %self.name,
                tool_call_id = %self.id,
                "model sent tool call with empty arguments; substituting {{}}"
            );
            serde_json::Value::Object(Default::default())
        } else {
            match serde_json::from_str(&self.args_buf) {
                Ok(v) => v,
                Err(parse_err) => match attempt_json_repair(&self.args_buf) {
                    Ok(v) => {
                        warn!(
                            tool_name = %self.name,
                            tool_call_id = %self.id,
                            "repaired invalid JSON arguments from model"
                        );
                        v
                    }
                    Err(_) => {
                        warn!(
                            tool_name = %self.name,
                            tool_call_id = %self.id,
                            args_buf = %self.args_buf,
                            error = %parse_err,
                            "model sent tool call with invalid JSON arguments; substituting {{}}"
                        );
                        serde_json::Value::Object(Default::default())
                    }
                },
            }
        };
        ToolCall {
            id: self.id,
            name: self.name,
            args,
        }
    }

    /// Try to parse without repair (the fast path during streaming).
    fn try_finalize(&self) -> Option<ToolCall> {
        let args: serde_json::Value = serde_json::from_str(&self.args_buf).ok()?;
        Some(ToolCall {
            id: self.id.clone(),
            name: self.name.clone(),
            args,
        })
    }
}

// ─── SlotState ────────────────────────────────────────────────────────────────

enum SlotState {
    /// Still receiving streaming chunks from the LLM.
    Accumulating(PendingSlot),
    /// Args complete and accumulated; ready to be drained by the caller.
    Ready(ToolCall),
}

// ─── ToolSlotManager ─────────────────────────────────────────────────────────

/// Accumulates per-slot tool-call JSON during LLM streaming.
///
/// Create one per `stream_one_turn` call.  Feed streaming chunks via
/// [`feed`].  After the stream finishes call [`finalize_remaining`] for any
/// slots whose JSON args were still incomplete.  Then call [`drain_calls`] to
/// collect all accumulated [`ToolCall`]s in slot-index order.
///
/// Tool execution is the responsibility of the HSM kernel (`ToolExecutor`);
/// this manager only accumulates and exposes the proposed calls.
pub(crate) struct ToolSlotManager {
    /// Per-slot state keyed by the parallel-tool-call index from the provider.
    slots: HashMap<u32, SlotState>,
}

impl ToolSlotManager {
    pub fn new(_registry: Arc<ToolRegistry>) -> Self {
        Self {
            slots: HashMap::new(),
        }
    }

    /// Feed a streaming chunk for `index`.
    ///
    /// Returns `Some(ToolCall)` the first time a slot's args form a valid JSON
    /// object so the caller can emit [`AgentEvent::ToolCallStarted`].  Returns
    /// `None` on every subsequent chunk for the same slot or when args are
    /// still accumulating.
    pub fn feed(&mut self, index: u32, id: &str, name: &str, args_chunk: &str) -> Option<ToolCall> {
        // If already ready (e.g. a stray trailing chunk), ignore.
        if matches!(self.slots.get(&index), Some(SlotState::Ready(_))) {
            return None;
        }

        let slot = self.slots.entry(index).or_insert_with(|| {
            SlotState::Accumulating(PendingSlot {
                id: String::new(),
                name: String::new(),
                args_buf: String::new(),
            })
        });

        match slot {
            SlotState::Accumulating(pending) => {
                if let Some(tc) = pending.feed(id, name, args_chunk) {
                    // Drop empty-name slots so they don't corrupt the history.
                    if tc.name.is_empty() {
                        return None;
                    }
                    let ready_tc = tc.clone();
                    *slot = SlotState::Ready(tc);
                    return Some(ready_tc);
                }
                None
            }
            SlotState::Ready(_) => None,
        }
    }

    /// Finalize any slots that are still accumulating after the LLM stream
    /// ends.  Returns the [`ToolCall`]s for newly dispatched slots so the
    /// caller can emit [`AgentEvent::ToolCallStarted`] for each.
    pub fn finalize_remaining(&mut self) -> Vec<ToolCall> {
        let mut dispatched = Vec::new();
        // Capture slot_count before iteration to avoid borrowing self.slots
        // both mutably (for the loop) and immutably (for .len()).
        let slot_count = self.slots.len();

        for state in self.slots.values_mut() {
            if matches!(state, SlotState::Accumulating(_)) {
                // Extract the pending slot by swapping a dummy in temporarily.
                let pending = match std::mem::replace(
                    state,
                    SlotState::Accumulating(PendingSlot {
                        id: String::new(),
                        name: String::new(),
                        args_buf: String::new(),
                    }),
                ) {
                    SlotState::Accumulating(p) => p,
                    _ => unreachable!(),
                };

                if pending.name.is_empty() {
                    warn!(
                        tool_call_id = %pending.id,
                        "dropping tool call with empty name; cannot dispatch"
                    );
                    continue;
                }

                let tc = pending.finalize();
                let tc_with_synthetic_id = ensure_non_empty_id(tc, slot_count);
                dispatched.push(tc_with_synthetic_id.clone());
                *state = SlotState::Ready(tc_with_synthetic_id);
            }
        }

        dispatched
    }

    /// Returns `true` when no tool calls arrived during the stream.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Insert a pre-built [`ToolCall`] directly (e.g. from the inline XML
    /// `<invoke>` fallback path).
    ///
    /// `index` must be unique per slot; use the call's position in the
    /// extracted list so ordering is preserved.
    pub fn insert_call(&mut self, index: u32, tc: ToolCall) {
        self.slots.insert(index, SlotState::Ready(tc));
    }

    /// Drain all ready tool calls, sorted by slot index.
    ///
    /// Returns `ToolCall`s in slot-index order. Tool execution is the
    /// responsibility of the HSM kernel (`ToolExecutor`); this method only
    /// returns the accumulated proposals.  Consumes `self`.
    pub fn drain_calls(self) -> Vec<ToolCall> {
        let mut calls: Vec<(u32, ToolCall)> = self
            .slots
            .into_iter()
            .filter_map(|(idx, state)| match state {
                SlotState::Ready(tc) => Some((idx, tc)),
                SlotState::Accumulating(_) => {
                    // finalize_remaining() should have been called first.
                    warn!(idx, "slot still accumulating at drain_calls; skipping");
                    None
                }
            })
            .collect();
        calls.sort_by_key(|(idx, _)| *idx);
        calls.into_iter().map(|(_, tc)| tc).collect()
    }

    /// Legacy compatibility shim — returns accumulated tool calls with empty
    /// (error) outputs. Real execution now happens in the HSM kernel.
    ///
    /// This method exists only so the legacy `Agent::run_agentic_loop_*` paths
    /// continue to compile; they are no longer invoked at runtime because
    /// `ConverseExecutor` is no longer wired into any `RuntimeBuilder`.
    pub async fn join_all(
        self,
        tx: &tokio::sync::mpsc::Sender<crate::events::AgentEvent>,
    ) -> Vec<(ToolCall, sven_tools::ToolOutput)> {
        let _ = tx;
        self.drain_calls()
            .into_iter()
            .map(|tc| {
                let output = sven_tools::ToolOutput::err(
                    &tc.id,
                    "tool execution is now kernel-mediated; legacy loop is inactive",
                );
                (tc, output)
            })
            .collect()
    }

    /// Legacy no-op — previously aborted in-flight tasks. No-op now since no
    /// tasks are spawned.
    #[allow(dead_code)]
    pub fn abort_all(self) {}
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn ensure_non_empty_id(mut tc: ToolCall, slot_count: usize) -> ToolCall {
    if tc.id.is_empty() {
        tc.id = format!("tc_synthetic_{slot_count}");
        warn!(
            tool_name = %tc.name,
            tool_call_id = %tc.id,
            "tool call from model had empty id; generated synthetic id"
        );
    }
    tc
}

/// Attempt to repair common JSON syntax errors in streaming tool arguments.
///
/// Mirrors the repair logic that previously lived in `PendingToolCall::finish`
/// in `agent.rs`.
pub(crate) fn attempt_json_repair(json_str: &str) -> anyhow::Result<serde_json::Value> {
    let fixed = fix_invalid_json_escapes(json_str);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&fixed) {
        return Ok(v);
    }

    let repaired = regex::Regex::new(r#""([^"]+)"([a-zA-Z_][a-zA-Z0-9_]*)":\s*"#)
        .unwrap()
        .replace_all(&fixed, r#""$1", "$2": "#);

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&repaired) {
        return Ok(v);
    }

    if !fixed.trim().ends_with('}') {
        let mut completed = fixed.clone();
        let quote_count = fixed.chars().filter(|&c| c == '"').count();
        if quote_count % 2 == 1 {
            completed.push('"');
        }
        if !completed.trim().ends_with('}') {
            completed.push('}');
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&completed) {
            return Ok(v);
        }
    }

    anyhow::bail!("JSON repair failed: all repair strategies exhausted")
}

fn fix_invalid_json_escapes(json_str: &str) -> String {
    let mut result = String::with_capacity(json_str.len() + 16);
    let mut chars = json_str.chars();
    let mut in_string = false;

    while let Some(c) = chars.next() {
        if in_string {
            match c {
                '\\' => match chars.next() {
                    Some(next)
                        if matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') =>
                    {
                        result.push('\\');
                        result.push(next);
                    }
                    Some(next) => {
                        result.push('\\');
                        result.push('\\');
                        result.push(next);
                    }
                    None => result.push('\\'),
                },
                '"' => {
                    in_string = false;
                    result.push('"');
                }
                _ => result.push(c),
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            result.push(c);
        }
    }
    result
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use sven_tools::{policy::ApprovalPolicy, tool::Tool, ToolOutput, ToolRegistry};

    // ── Mock tool ─────────────────────────────────────────────────────────────

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
            ToolOutput::ok(&call.id, call.args.to_string())
        }
    }

    fn make_registry() -> Arc<ToolRegistry> {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool);
        Arc::new(reg)
    }

    // ── feed() / single slot ──────────────────────────────────────────────────

    #[test]
    fn feed_incomplete_json_returns_none() {
        // No dispatch, so no tokio runtime needed.
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        let result = mgr.feed(0, "id1", "echo", r#"{"x":"#);
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn feed_complete_json_dispatches() {
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        let result = mgr.feed(0, "id1", "echo", r#"{"x":1}"#);
        assert!(result.is_some());
        let tc = result.unwrap();
        assert_eq!(tc.id, "id1");
        assert_eq!(tc.name, "echo");
        assert_eq!(tc.args, json!({"x": 1}));
    }

    #[tokio::test]
    async fn feed_multi_chunk_dispatches_on_complete() {
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        // First chunk: incomplete
        assert!(mgr.feed(0, "id1", "echo", r#"{"x":"#).is_none());
        // Second chunk: still incomplete
        assert!(mgr.feed(0, "", "", r#"1"#).is_none());
        // Third chunk: completes the object
        let result = mgr.feed(0, "", "", "}");
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn feed_second_dispatch_for_same_slot_ignored() {
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        // First dispatch
        assert!(mgr.feed(0, "id1", "echo", r#"{"x":1}"#).is_some());
        // Stray trailing chunk for the same slot - must be ignored.
        assert!(mgr.feed(0, "", "", " ").is_none());
    }

    // ── Multiple parallel slots ───────────────────────────────────────────────

    #[tokio::test]
    async fn feed_two_slots_independent() {
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        assert!(mgr.feed(0, "id0", "echo", r#"{"#).is_none());
        assert!(mgr.feed(1, "id1", "echo", r#"{"y":2}"#).is_some()); // slot 1 ready first
        assert!(mgr.feed(0, "", "", r#""z":3}"#).is_some()); // slot 0 now ready
    }

    // ── finalize_remaining() ──────────────────────────────────────────────────

    #[tokio::test]
    async fn finalize_remaining_dispatches_incomplete_slot() {
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        mgr.feed(0, "id1", "echo", r#"{"x":1"#); // no closing brace
        let dispatched = mgr.finalize_remaining();
        // The repair path should recover the truncated JSON.
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].id, "id1");
    }

    #[test]
    fn finalize_remaining_skips_already_ready() {
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        mgr.feed(0, "id1", "echo", r#"{"x":1}"#); // already ready
        let dispatched = mgr.finalize_remaining();
        assert!(dispatched.is_empty());
    }

    #[tokio::test]
    async fn finalize_remaining_skips_empty_name() {
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        // Feed without a name - should be dropped during finalize.
        mgr.feed(0, "id1", "", r#"{"x":1}"#);
        // (The slot stays Accumulating because feed() guards on empty name at dispatch.)
        let dispatched = mgr.finalize_remaining();
        // The slot has no name, so it should be dropped with a warning.
        assert!(dispatched.is_empty());
    }

    // ── is_empty() ────────────────────────────────────────────────────────────

    #[test]
    fn is_empty_when_no_feeds() {
        let reg = make_registry();
        let mgr = ToolSlotManager::new(reg);
        assert!(mgr.is_empty());
    }

    #[test]
    fn not_empty_after_feed() {
        // Only partial accumulation, no dispatch → no tokio runtime needed.
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        mgr.feed(0, "id1", "echo", r#"{"#);
        assert!(!mgr.is_empty());
    }

    // ── drain_calls() ─────────────────────────────────────────────────────────

    #[test]
    fn drain_calls_single_slot_returns_tool_call() {
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        mgr.feed(0, "id1", "echo", r#"{"x":1}"#);
        let calls = mgr.drain_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "id1");
    }

    #[test]
    fn drain_calls_two_slots_ordered_by_index() {
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        // Feed slot 1 first (it arrives complete before slot 0).
        mgr.feed(1, "id1", "echo", r#"{"y":2}"#);
        mgr.feed(0, "id0", "echo", r#"{"x":1}"#);
        let calls = mgr.drain_calls();
        assert_eq!(calls.len(), 2);
        // Must be returned in slot-index order (0 then 1).
        assert_eq!(calls[0].id, "id0");
        assert_eq!(calls[1].id, "id1");
    }

    // ── abort_all() ───────────────────────────────────────────────────────────

    #[test]
    fn abort_all_does_not_panic() {
        let reg = make_registry();
        let mut mgr = ToolSlotManager::new(reg);
        mgr.feed(0, "id1", "echo", r#"{"x":1}"#);
        // abort_all is a no-op now; should not panic.
        mgr.abort_all();
    }

    // ── JSON repair ───────────────────────────────────────────────────────────

    #[test]
    fn attempt_json_repair_completes_truncated_object() {
        let v = attempt_json_repair(r#"{"x":1"#).unwrap();
        assert_eq!(v["x"], json!(1));
    }

    #[test]
    fn attempt_json_repair_fixes_invalid_escape() {
        let v = attempt_json_repair(r#"{"path":"\c"}"#).unwrap();
        assert_eq!(v["path"], json!("\\c"));
    }

    #[test]
    fn attempt_json_repair_returns_err_on_unrecoverable() {
        assert!(attempt_json_repair("not json at all ~~~").is_err());
    }

    // ── Adversarial JSON repair inputs ────────────────────────────────────────

    #[test]
    fn adversarial_deeply_nested_json_does_not_stack_overflow() {
        // Build 500 levels of nesting: {"a":{"a":{"a": ... }}}
        let open: String = r#"{"a":"#.repeat(500);
        let close: String = "}".repeat(500);
        let deeply_nested = format!("{open}1{close}");
        // Must return a result (Ok or Err) without panicking/stack overflowing.
        let _ = attempt_json_repair(&deeply_nested);
    }

    #[test]
    fn adversarial_100kb_string_value_does_not_panic() {
        let big_val = "x".repeat(100_000);
        let input = format!(r#"{{"key":"{big_val}"}}"#);
        // Valid JSON with a huge string - repair should succeed.
        let result = attempt_json_repair(&input);
        assert!(
            result.is_ok(),
            "100 KB string value should parse: {:?}",
            result
        );
    }

    #[test]
    fn adversarial_mismatched_open_brackets_does_not_panic() {
        for payload in ["{{{", "}}}", "[{]}", "[[[["] {
            let _ = attempt_json_repair(payload);
        }
    }

    #[test]
    fn adversarial_multiple_concatenated_objects_handled() {
        // Two valid objects concatenated - not valid JSON; repair must not panic.
        let _ = attempt_json_repair(r#"{"a":1}{"b":2}"#);
    }

    #[test]
    fn adversarial_trailing_garbage_after_valid_object() {
        // Valid object followed by garbage - serde_json treats trailing bytes as
        // an error, so repair may fail, but must not panic.
        let _ = attempt_json_repair(r#"{"a":1} GARBAGE TEXT"#);
    }

    #[test]
    fn adversarial_only_whitespace_returns_err() {
        assert!(attempt_json_repair("   \t\n  ").is_err());
    }

    #[test]
    fn adversarial_empty_string_returns_err() {
        assert!(attempt_json_repair("").is_err());
    }

    #[test]
    fn adversarial_unicode_null_in_string_does_not_panic() {
        // JSON strings can contain \u0000; the parser must handle it.
        let _ = attempt_json_repair(r#"{"key":"\u0000"}"#);
    }

    #[test]
    fn adversarial_regex_dos_unclosed_string_does_not_hang() {
        // A very long string without a closing quote; the repair regex must not
        // catastrophically backtrack.
        let payload = format!(r#"{{"x":"{}"#, "a".repeat(50_000));
        let _ = attempt_json_repair(&payload);
    }
}
