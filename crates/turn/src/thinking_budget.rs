// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that stop a runaway model
// without ending an honest one for its clients. If your team needs expertise
// in supervising long-running LLM reasoning then you can procure our services
// by sending an email to info@swedishembedded.com.
//! The thinking-loop watchdog's settings: how a runaway reasoning loop is
//! stopped, the process-wide override `/think-limit` sets, and the markers
//! an aborted turn's reason begins with.

use std::time::Duration;

/// How a runaway reasoning loop is stopped (observed with some local
/// reasoning models, e.g. Qwen, which repeat themselves instead of answering).
///
/// Thinking is part of the response, so it is bounded like the rest of it: by
/// the request's output budget (the window less the prompt), which the server
/// enforces. There is deliberately no token cap by default - any default low
/// enough to matter ended honest runs mid-thought. A loop is caught by what
/// marks it:
///
/// - **Repetition**: the reasoning repeats a stretch of itself verbatim
///   (see `RepetitionWatch`).
/// - **Time cap**, opt-in: time spent thinking with no text or tool call.
///   Not on by default: wall-clock time measures the server's throughput,
///   not a loop - 600 s at 15 tokens/s per stream ended honest reasoning.
///   A server that stops sending is the stalled-stream watch's case.
/// - **Token cap**, opt-in: estimated thinking tokens (chars/4) per call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThinkingBudget {
    /// Opt-in cap on estimated thinking tokens. `None` (the default) sets no
    /// cap: the request's output budget is the bound.
    pub max_thinking_tokens: Option<u32>,
    /// Opt-in cap on time spent thinking without text or a tool call.
    /// `None` (the default) sets none.
    pub thinking_timeout_secs: Option<u64>,
}

impl ThinkingBudget {
    /// The effective `(token_cap, time_cap)`; `None` is no cap.
    pub(crate) fn resolve(self) -> (Option<u32>, Option<Duration>) {
        (
            self.max_thinking_tokens,
            self.thinking_timeout_secs.map(Duration::from_secs),
        )
    }
}

/// How the reason of a reasoning-loop abort begins. A headless run that one
/// ends prints it as `[sven:error] reasoning loop: ...`: a stable marker an
/// examiner reads to record the loop as its own outcome, apart from a budget
/// exhaustion or a time limit.
pub const REASONING_LOOP_PREFIX: &str = "reasoning loop:";

/// How the reason of a tool-call-loop abort begins: the arguments of one call
/// repeated the same block until the reply would have run to its limit. Read
/// like [`REASONING_LOOP_PREFIX`], as its own outcome.
pub const TOOL_CALL_LOOP_PREFIX: &str = "tool-call loop:";

/// Rough token estimate matching the TUI's live streaming counter
/// (`crates/tui/src/app/agent_events.rs`) - chars/4, not a real tokenizer.
/// Good enough for a loop-prevention ceiling; not used for billing.
pub(crate) fn estimate_tokens(text: &str) -> u32 {
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
/// thinking-budget override. See `THINKING_BUDGET_OVERRIDE`.
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
