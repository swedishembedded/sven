// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared tool-call vocabulary.
//!
//! [`ToolCall`], [`ToolOutput`]/[`ToolOutputPart`], [`ToolSchema`], and
//! [`OutputCategory`] are pure data types with no behavior — no execution,
//! no registry, no permission policy. They exist here, below `sven-tools`,
//! so that crates which only need to *name* a tool call or its result (a
//! wire protocol, a control protocol) don't have to depend on the full
//! tool-implementation crate to do it.
//!
//! `sven-tools` re-exports all of these at its crate root, so existing
//! `sven_tools::ToolCall` (etc.) call sites are unaffected by where the
//! types are actually defined.
//!
//! [`SessionEvent`] is the unified session-event stream (re-exported as
//! `sven_machines::AgentEvent` and `sven_hsm::UiEvent`) and its payload types
//! ([`AgentMode`], [`TodoItem`], [`SubagentUpdate`], [`CollabEvent`],
//! [`PeerInfo`], [`CompactionStrategyUsed`]) live here for the same reason.
//!
//! [`provenance`] holds the origin vocabulary of the continuous-learning loop
//! ([`FactSource`](provenance::FactSource) and friends) - here, and not beside
//! the tool that consumes it, because the crates on both ends of that loop are
//! siblings that cannot depend on one another. See the module's own docs.

use serde_json::Value;

pub mod provenance;

use provenance::FactSource;

/// A single tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    /// Opaque identifier returned by the model (forwarded verbatim)
    pub id: String,
    pub name: String,
    /// Parsed JSON arguments
    pub args: Value,
}

/// A single content item in a rich tool output.
///
/// Most tools produce only `Text`.  Multimodal tools (e.g. `read_image`,
/// `attach_file`) may produce a mix of `Text`, `Image`, and `Audio` items.
#[derive(Debug, Clone)]
pub enum ToolOutputPart {
    /// Plain UTF-8 text.
    Text(String),
    /// Base64 data URL: `data:<mime>;base64,<b64>`.
    Image(String),
    /// Base64 audio data URL: `data:audio/wav;base64,<b64>`.
    Audio(String),
}

/// The result of executing a tool.
///
/// ## Backward compatibility
/// `content` is always the plain-text representation of the output (the
/// concatenation of all `Text` parts).  Existing tools and tests that only
/// access `content` continue to work unchanged.
///
/// ## Image support
/// Tools that return images populate `parts` with a mix of [`ToolOutputPart::Text`]
/// and [`ToolOutputPart::Image`] items.  The agent maps these into the
/// appropriate `sven_model::ToolResultContent` variant when building the
/// conversation history.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub call_id: String,
    /// Plain-text content - concatenation of all Text parts.
    /// Always set; always readable.  Backward-compatible field.
    pub content: String,
    /// Structured parts (text and/or images).  For tools that only return
    /// text this contains exactly one `Text` part mirroring `content`.
    pub parts: Vec<ToolOutputPart>,
    /// If true, the tool execution failed non-fatally (returned error message).
    pub is_error: bool,
    /// Where this result came from, when the tool itself resolved information
    /// rather than just acting on model-supplied arguments.
    ///
    /// Set only by resolving tools (`web_fetch`, `web_search`, `ask_question`,
    /// `ingest_document`) - never by the model, and never a place `assimilate_
    /// fact` reads directly. It exists so the impure I/O layer that runs the
    /// tool (never the tool itself, and never a `Machine`) can record it into
    /// the shared provenance index the model later cites by this call's own
    /// id. See [`provenance::ProvenanceSink`].
    ///
    /// Boxed: `FactSource` carries several `String`s and a `Range`, and every
    /// `ToolOutput` (most of which never attach provenance at all) would
    /// otherwise pay for the largest variant's size.
    pub provenance: Option<Box<FactSource>>,
    /// Set instead of a real result when the tool cannot produce one without
    /// asking a human, and the answer may not arrive soon - see
    /// [`Self::parked`]. `content`/`is_error` carry no meaning on a parked
    /// output; a caller must check this field first.
    ///
    /// Boxed for the same reason as `provenance`: most `ToolOutput`s are
    /// never parked, so they shouldn't pay for `ParkedAnswer`'s size (it
    /// pushed the unboxed struct over clippy's `result_large_err` threshold).
    pub parked: Option<Box<ParkedAnswer>>,
}

/// A question a tool could not answer itself, parked for a human.
///
/// Distinct from returning an error: an error says the call failed, while a
/// parked output says the call has not concluded *yet* and must not be
/// scored, retried, or treated as a completed turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedAnswer {
    /// The question text shown to the human.
    pub prompt: String,
    /// Offered choices, if any (empty for a free-form question).
    pub options: Vec<String>,
}

impl ToolOutput {
    /// Successful plain-text result.
    pub fn ok(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        let text = content.into();
        let call_id = call_id.into();
        Self {
            call_id,
            content: text.clone(),
            parts: vec![ToolOutputPart::Text(text)],
            is_error: false,
            provenance: None,
            parked: None,
        }
    }

    /// Error result containing a plain-text error message.
    pub fn err(call_id: impl Into<String>, msg: impl Into<String>) -> Self {
        let text = msg.into();
        let call_id = call_id.into();
        Self {
            call_id,
            content: text.clone(),
            parts: vec![ToolOutputPart::Text(text)],
            is_error: true,
            provenance: None,
            parked: None,
        }
    }

    /// The call cannot be answered without a human, and the answer may not
    /// arrive soon. `content`/`parts`/`is_error` are set to an explanatory
    /// placeholder for any caller that has not been updated to check
    /// [`Self::parked`] first (never read on the parking path itself).
    pub fn parked(call_id: impl Into<String>, prompt: impl Into<String>, options: Vec<String>) -> Self {
        let prompt = prompt.into();
        let placeholder = format!("(parked pending a human answer: {prompt})");
        Self {
            call_id: call_id.into(),
            content: placeholder.clone(),
            parts: vec![ToolOutputPart::Text(placeholder)],
            is_error: false,
            provenance: None,
            parked: Some(Box::new(ParkedAnswer { prompt, options })),
        }
    }

    /// Attaches this result's provenance. Only a resolving tool's own
    /// construction site should call this - see [`Self::provenance`].
    #[must_use]
    pub fn with_provenance(mut self, source: FactSource) -> Self {
        self.provenance = Some(Box::new(source));
        self
    }

    /// Result with arbitrary parts (text and/or images).
    ///
    /// `content` is set to the concatenation of all Text parts.
    pub fn with_parts(call_id: impl Into<String>, parts: Vec<ToolOutputPart>) -> Self {
        let text = parts
            .iter()
            .filter_map(|p| match p {
                ToolOutputPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            call_id: call_id.into(),
            content: text,
            parts,
            is_error: false,
            provenance: None,
            parked: None,
        }
    }

    /// Return `true` if this output contains at least one image part.
    pub fn has_images(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, ToolOutputPart::Image(_)))
    }

    /// Return `true` if this output contains at least one audio part.
    pub fn has_audio(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, ToolOutputPart::Audio(_)))
    }
}

#[cfg(test)]
mod tool_output_tests {
    use super::*;

    #[test]
    fn ok_err_and_with_parts_carry_no_provenance_by_default() {
        assert!(ToolOutput::ok("c1", "hi").provenance.is_none());
        assert!(ToolOutput::err("c1", "boom").provenance.is_none());
        assert!(ToolOutput::with_parts("c1", vec![ToolOutputPart::Text("hi".into())])
            .provenance
            .is_none());
    }

    #[test]
    fn with_provenance_attaches_the_given_fact_source() {
        let out = ToolOutput::ok("c1", "hi").with_provenance(FactSource::UserStated);
        assert_eq!(out.provenance, Some(Box::new(FactSource::UserStated)));
    }

    #[test]
    fn parked_carries_the_question_and_is_not_an_error() {
        let out = ToolOutput::parked("c1", "Which framework?", vec!["Axum".into(), "Actix".into()]);
        assert!(!out.is_error, "a parked call has not failed - it has not concluded");
        assert_eq!(
            out.parked,
            Some(Box::new(ParkedAnswer {
                prompt: "Which framework?".into(),
                options: vec!["Axum".into(), "Actix".into()],
            }))
        );
    }

    #[test]
    fn ok_err_and_with_parts_are_never_parked() {
        assert!(ToolOutput::ok("c1", "hi").parked.is_none());
        assert!(ToolOutput::err("c1", "boom").parked.is_none());
        assert!(ToolOutput::with_parts("c1", vec![ToolOutputPart::Text("hi".into())])
            .parked
            .is_none());
    }
}

/// Describes the shape of a tool's text output for context-aware truncation.
///
/// When a tool result exceeds the configured token cap, `sven-core` uses
/// this category to pick the right extraction strategy.  Each tool declares
/// its own category; `sven-core` never hard-codes tool names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputCategory {
    /// Terminal / process output: keep the first 60 + last 40 lines so both
    /// the command preamble and the final result are visible.
    /// Suitable for: shell, run_terminal_command, gdb commands.
    HeadTail,
    /// Ordered match list: keep the leading matches so the model sees the
    /// highest-relevance results first.
    /// Suitable for: grep, search_codebase, read_lints.
    MatchList,
    /// File content: keep a head and tail window with a separator so the
    /// model sees both the top of the file (imports, declarations) and the
    /// end (recent changes).
    /// Suitable for: read_file, fs read operations.
    FileContent,
    /// Generic text: hard-truncate at the character boundary.
    /// Used for all tools that do not fit the categories above.
    #[default]
    Generic,
}

/// A tool schema - mirrors `sven_model::ToolSchema` but keeps this crate
/// independent from the model-provider crate.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    /// Whether this tool comes from an external MCP server.
    ///
    /// MCP tools are placed after core tools in the prompt and get their own
    /// Anthropic cache breakpoint (BP2) so that toggling servers only
    /// invalidates the MCP section, not the stable core tools section (BP1).
    pub is_mcp: bool,
}

// ─── Session/event vocabulary ──────────────────────────────────────────────
//
// Pure data types named by the (to-be-unified) agent/UI event streams. They
// live here, below `sven-config`/`sven-core`/`sven-tools`, so a future
// `SessionEvent` enum in this same crate can name them without pulling in
// config schema parsing, the machine implementations, or tool execution.

/// The agent's current operating mode, selectable via `--mode`/`/mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    /// Pure research - read-only tools, no writes
    Research,
    /// Generate a structured plan, no code changes
    Plan,
    /// Full agent with read/write tools
    Agent,
    /// Conversational chat mode (HSM ReactiveAgentMachine in chat mode)
    Chat,
    /// Software development lifecycle mode (HSM SdlcMachine)
    Sdlc,
}

impl std::fmt::Display for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentMode::Research => write!(f, "research"),
            AgentMode::Plan => write!(f, "plan"),
            AgentMode::Agent => write!(f, "agent"),
            AgentMode::Chat => write!(f, "chat"),
            AgentMode::Sdlc => write!(f, "sdlc"),
        }
    }
}

/// Which compaction strategy was executed when context compaction fired.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CompactionStrategyUsed {
    /// Structured Markdown checkpoint with typed sections.
    Structured,
    /// Legacy free-form narrative summary.
    Narrative,
    /// Emergency fallback: history was dropped without a model summary call
    /// because the session was too large to fit even a compaction prompt.
    Emergency,
}

impl std::fmt::Display for CompactionStrategyUsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactionStrategyUsed::Structured => write!(f, "structured"),
            CompactionStrategyUsed::Narrative => write!(f, "narrative"),
            CompactionStrategyUsed::Emergency => write!(f, "emergency"),
        }
    }
}

/// Information about a connected peer (node proxy / list_peers).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    pub name: String,
    pub peer_id: String,
    pub connected: bool,
    pub can_delegate: bool,
}

/// The lifecycle state of a [`TodoItem`].
///
/// Serialises as the lowercase snake_case string the LLM expects
/// (`"pending"`, `"in_progress"`, `"completed"`, `"cancelled"`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl TodoStatus {
    /// Icon used in single-line todo summaries.
    pub fn icon(&self) -> &'static str {
        match self {
            TodoStatus::Completed => "✓",
            TodoStatus::InProgress => "→",
            TodoStatus::Cancelled => "✗",
            TodoStatus::Pending => "○",
        }
    }
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Cancelled => "cancelled",
        };
        f.write_str(s)
    }
}

/// A structured todo item managed by the todo tool.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

/// A structured event streamed from a subagent over ACP.
///
/// This is a sven-native mirror of ACP `SessionUpdate` variants, kept
/// dependency-free so callers do not need to depend on the ACP crate.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SubagentUpdate {
    /// A chunk of assistant text (streamed).
    TextDelta(String),
    /// A chunk of thinking/reasoning text (streamed).
    ThinkingDelta(String),
    /// The subagent started a tool call.
    ToolCallStarted {
        id: String,
        name: String,
        args: Value,
    },
    /// A subagent tool call completed.
    ToolCallFinished {
        id: String,
        name: String,
        output: String,
        is_error: bool,
    },
    /// The subagent's turn is complete; `final_text` is the accumulated
    /// assistant response that the parent agent should use as the task result.
    Finished { final_text: String },
    /// The subagent timed out or terminated with an error.
    Failed { reason: String },
    /// Token usage / cost from the subagent (when API reports it, e.g. OpenRouter).
    TokenUsage { cost_usd: f64 },
}

/// A collaboration event that can be recorded as a `ChatSegment::CollabEvent`.
///
/// These are display-only entries that track the lifecycle of team operations
/// without adding them to the LLM context.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CollabEvent {
    TeammateSpawned {
        name: String,
        role: String,
    },
    TaskDelegated {
        task_id: String,
        to_name: String,
        task_title: String,
    },
    WaitingForTeammates {
        names: Vec<String>,
    },
    TeammateFinished {
        name: String,
        task_id: String,
        /// `"completed"`, `"failed"`, or `"cancelled"`.
        status: String,
    },
    TeammateMessage {
        from: String,
        /// First ~60 chars of the message (inline preview).
        preview: String,
    },
    TeammateIdle {
        name: String,
    },
    TeamCreated {
        team_name: String,
    },
    TeamCleanedUp {
        team_name: String,
    },
    PlanSubmitted {
        name: String,
        task_id: String,
    },
    PlanApproved {
        name: String,
        task_id: String,
    },
    PlanRejected {
        name: String,
        task_id: String,
        feedback: String,
    },
}

/// The single, unified session event stream.
///
/// Historically the codebase had two parallel enums for this: `AgentEvent`
/// (typed payloads, produced by the legacy `sven_machines::Agent` loop) and
/// `UiEvent` (opaque `Value`/`String` payloads, produced by the HSM kernel's
/// outward observation plane). A translator pair converted between them on
/// every event, and because both were hand-maintained, they had already
/// diverged (`ui_event_to_agent_event`'s `_ => None` silently dropped
/// variants no one had gotten around to mapping).
///
/// `SessionEvent` replaces both: it carries `AgentEvent`'s typed payloads
/// (so consumers pattern-match on real types, not `Value`) plus
/// [`Transition`](SessionEvent::Transition), the kernel's per-dispatch trace
/// event that had no `AgentEvent` equivalent. `sven-core` re-exports this as
/// `AgentEvent` and `sven-hsm` re-exports it as `UiEvent`, so both names
/// still resolve for existing call sites — see `docs/adr/` for the full
/// migration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SessionEvent {
    /// A text chunk streamed from the model.
    TextDelta(String),
    /// A complete text response from the model (after streaming finishes).
    TextComplete(String),
    /// A thinking/reasoning chunk from the model (extended thinking API).
    /// Consumers should accumulate deltas and finalise them into a Thinking
    /// segment when the model signals the end of the reasoning block.
    ThinkingDelta(String),
    /// A complete thinking/reasoning block (accumulated from `ThinkingDelta`).
    ThinkingComplete(String),
    /// The model has requested a tool call.
    ToolCallStarted(ToolCall),
    /// A tool call finished.
    ToolCallFinished {
        call_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
    /// A long-running tool is reporting incremental progress. Consumers
    /// should update the spinner/status bar without adding a chat segment.
    ToolProgress { call_id: String, message: String },
    /// Context was compacted; statistics for the UI.
    ContextCompacted {
        tokens_before: usize,
        tokens_after: usize,
        strategy: CompactionStrategyUsed,
        /// Agentic loop round in which compaction fired (0 = pre-submit).
        turn: u32,
    },
    /// Current token usage update.
    ///
    /// Providers may emit this multiple times per turn with different fields
    /// populated (e.g. Anthropic sends input stats on `message_start` and
    /// output stats on `message_delta`). Fields not reported for this
    /// particular event are zero; consumers should only update their display
    /// when the relevant field is non-zero.
    TokenUsage {
        /// Input tokens processed this request (does NOT include cache hits).
        input: u32,
        /// Output tokens generated this request.
        output: u32,
        /// Tokens served from the provider's prompt cache this turn.
        cache_read: u32,
        /// Tokens written into the provider's prompt cache this turn.
        cache_write: u32,
        /// Running total of cache-read tokens across the whole session.
        cache_read_total: u32,
        /// Running total of cache-write tokens across the whole session.
        cache_write_total: u32,
        /// The model's maximum context window (tokens). Zero means unknown.
        max_tokens: usize,
        /// The model's maximum output tokens per completion. Zero means unknown.
        max_output_tokens: usize,
        /// Cost in USD when reported by the API (e.g. OpenRouter).
        cost_usd: Option<f64>,
    },
    /// The agent has finished processing the current user turn.
    TurnComplete,
    /// The current run was aborted (via Ctrl+C or /abort). `partial_text`
    /// contains any assistant text that was streamed before the abort; it
    /// may be empty if the model had not yet produced any output.
    Aborted { partial_text: String },
    /// A recoverable error occurred.
    Error(String),
    /// The todo list was updated.
    TodoUpdate(Vec<TodoItem>),
    /// The agent mode was changed.
    ModeChanged(AgentMode),
    /// The active model was changed by the agent tool. The string is a
    /// resolved `"provider/id"` identifier (e.g. `"anthropic/claude-opus-4-6"`).
    ModelChanged(String),
    /// The agent is asking the user a question (id links to `QuestionAnswer`).
    Question { id: String, questions: Vec<String> },
    /// Answer to a previous `Question` event.
    QuestionAnswer { id: String, answer: String },
    /// Chat title generated from the first user message (LLM, low max_tokens).
    TitleGenerated(String),
    /// A team lifecycle event to be shown in the chat as a collapsible segment.
    CollabEvent(CollabEvent),
    /// A completed delegate subtree - rendered as a collapsible summary segment.
    DelegateSummary {
        to_name: String,
        task_title: String,
        duration_ms: u64,
        status: String,
        result_preview: String,
    },
    /// A subagent was started via the task tool; the frontend creates a child
    /// session view.
    SubagentStarted {
        call_id: String,
        handle_id: String,
        description: String,
        /// Full prompt sent to the subagent; shown as the first user message.
        prompt: String,
    },
    /// A structured event streamed from a running subagent.
    SubagentEvent {
        /// Tool-call ID of the spawning `task` call (matches `ToolCallStarted`).
        call_id: String,
        /// Buffer handle identifying which subagent session this belongs to.
        handle_id: String,
        update: SubagentUpdate,
    },
    /// List of peers (from node proxy / list_peers).
    PeerList(Vec<PeerInfo>),
    /// The HSM kernel took a transition (full transition trace). Has no
    /// legacy `AgentEvent` equivalent; only the kernel's observation plane
    /// ever produced it.
    Transition {
        /// State label before the dispatch.
        from: String,
        /// State label after the dispatch.
        to: String,
        /// The kind of event dispatched.
        event: String,
    },
}
