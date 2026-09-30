// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! ATIF-trajectory-backed session storage — sven's session persistence
//! format. [`crate::chat_document`]'s YAML `ChatDocument` is read-only legacy
//! input that [`import_legacy_chat_document`] converts when it is opened.
//!
//! Everything here is built on top of the standalone `atif` crate (ATIF
//! v1.7 model, validator, and persistence helpers).
//!
//! # Layout
//!
//! - [`SvenSessionMeta`] — sven-specific session metadata (title, status,
//!   mode, parent link, timestamps) that has no dedicated ATIF field, stored
//!   under `Trajectory.extra.sven`.
//! - [`ChatUsage`]-to-[`atif::FinalMetrics`] mapping helpers.
//! - [`StepAssembler`] — the turn assembler: folds a flat stream of user
//!   messages / assistant text / tool calls / tool results / thinking /
//!   context-compaction events into turn-shaped [`atif::TraceStep`]s, and
//!   the reverse ([`steps_to_messages`]) for reseeding an agent's history.
//! - File I/O built directly on `atif::persist` (new `sessions/` directory,
//!   `.json` extension, deliberately separate from the legacy `chats/`
//!   directory so old and new files never collide in the same listing).
//! - [`import_legacy_chat_document`] — one-way YAML `ChatDocument` → ATIF
//!   `Trajectory` importer for opening a user's pre-existing session.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use atif::{
    AgentProfile, ContentSegment, ContextManagement, FinalMetrics, MessageBody, ObservationEntry,
    StepObservation, StepOrigin, SubagentRef, ToolInvocation, TraceStep, Trajectory,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sven_model::{FunctionCall, Message, MessageContent, Role};

use crate::chat_document;
use crate::chat_document::{json_str_to_yaml, TurnRecord};
use crate::chat_document::{ChatDocument, ChatStatus, ChatUsage};
use crate::conversation::ConversationRecord;

/// The ATIF schema version string this module writes and expects to read.
pub const ATIF_SCHEMA_VERSION: &str = "ATIF-v1.7";

/// Default `AgentProfile.name` for trajectories sven produces.
pub const AGENT_NAME: &str = "sven";

/// Build a default [`AgentProfile`] for a fresh trajectory: `name = "sven"`,
/// `version` = this crate's own package version, which is `version.workspace
/// = true` in `crates/session-store/Cargo.toml` — i.e. the sven release
/// version (`[workspace.package].version` in the root `Cargo.toml`), not an
/// independently-versioned library crate. Callers embedding a *different*
/// binary's trajectory (e.g. a subagent that could in principle run a
/// different sven build) should still build their own `AgentProfile` instead
/// of relying on this default; this is the reasonable default for the common
/// case where the running process is that binary.
pub fn default_agent_profile() -> AgentProfile {
    AgentProfile::new(AGENT_NAME, env!("CARGO_PKG_VERSION"))
}

// ── Session metadata: extra.sven ────────────────────────────────────────────

/// sven-specific session metadata with no dedicated slot in the ATIF core
/// schema, namespaced under `Trajectory.extra.sven` (a nested JSON object)
/// so a generic ATIF consumer can tell sven's bespoke fields apart from
/// anything else stashed in `extra`.
///
/// Fields that *do* have a native ATIF home are NOT duplicated here — see
/// the module docs: `model` lives on `Trajectory.agent.model_name`, and
/// usage/cost live on `Trajectory.final_metrics` (via
/// [`chat_usage_to_final_metrics`] / [`final_metrics_to_chat_usage`]).
///
/// # Parent/child asymmetry
///
/// `parent_session_id` here is a convenience back-reference on a *child*
/// trajectory, kept for O(1) "what's my parent" lookups without scanning
/// every session file on disk — this is the field the TUI reads to link a
/// child session to its parent. It intentionally duplicates
/// information that could, in principle, be derived by scanning every
/// parent's `subagent_trajectories`/observation refs for one that points
/// back at this session.
///
/// ATIF's own native mechanism for this relationship points the *other*
/// way: a parent references its children via `subagent_trajectories` +
/// `SubagentTrajectoryRef`, designed for single-file embedding (see
/// [`StepAssembler::push_subagent_embedded`], used by the CI runner today).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SvenSessionMeta {
    /// Human-readable title, generated from first user message or model API.
    pub title: String,
    /// Session lifecycle status.
    #[serde(default)]
    pub status: ChatStatus,
    /// Agent mode used (e.g. "agent", "code", "research").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// `session_id` of the parent session when this is a subagent task
    /// conversation; `None` for root sessions. See the struct doc comment
    /// for why this duplicates the spec-native `SubagentTrajectoryRef`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// When this session was first created (UTC).
    pub created_at: DateTime<Utc>,
    /// When this session was last saved (UTC).
    pub updated_at: DateTime<Utc>,
}

impl SvenSessionMeta {
    /// The key this metadata nests under inside `Trajectory.extra`.
    pub const EXTRA_KEY: &'static str = "sven";

    /// Construct fresh metadata: `status = Active`, no mode/parent, both
    /// timestamps set to now.
    pub fn new(title: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            title: title.into(),
            status: ChatStatus::default(),
            mode: None,
            parent_session_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Touch `updated_at` to the current time.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Extract sven's metadata from `trajectory.extra.sven`, if present and
    /// well-formed. Returns `None` for a trajectory with no `extra`, no
    /// `extra.sven` key, or a malformed value there.
    pub fn from_trajectory(trajectory: &Trajectory) -> Option<Self> {
        let extra = trajectory.extra.as_ref()?;
        let sven_value = extra.get(Self::EXTRA_KEY)?;
        serde_json::from_value(sven_value.clone()).ok()
    }

    /// Insert (or replace) this metadata at `trajectory.extra.sven`,
    /// creating `extra` as a JSON object if it was absent or not an object.
    /// Other keys already present under `extra` are preserved.
    pub fn apply_to_trajectory(&self, trajectory: &mut Trajectory) {
        let extra = trajectory
            .extra
            .get_or_insert_with(|| Value::Object(Map::new()));
        if !extra.is_object() {
            *extra = Value::Object(Map::new());
        }
        let value = serde_json::to_value(self).expect("SvenSessionMeta always serializes");
        extra
            .as_object_mut()
            .expect("just ensured object")
            .insert(Self::EXTRA_KEY.to_string(), value);
    }
}

// ── Usage ⇄ final_metrics mapping ───────────────────────────────────────────

/// Map a legacy [`ChatUsage`] onto ATIF's native `Trajectory.final_metrics`.
///
/// `total_cache_write_tokens` has no dedicated ATIF field, so it is placed
/// at `final_metrics.extra.total_cache_write_tokens` (the spec explicitly
/// allows `extra` there for exactly this: a producer metric not covered by
/// the core schema). Omitted from `extra` when zero, to match `ChatUsage`'s
/// own `is_empty()` convention of not cluttering output with zero values.
pub fn chat_usage_to_final_metrics(usage: &ChatUsage) -> FinalMetrics {
    let mut metrics = FinalMetrics {
        total_prompt_tokens: Some(usage.total_input_tokens),
        total_completion_tokens: Some(usage.total_output_tokens),
        total_cached_tokens: Some(usage.total_cache_read_tokens),
        total_cost_usd: Some(usage.total_cost_usd),
        ..Default::default()
    };
    if usage.total_cache_write_tokens > 0 {
        metrics.extra = Some(serde_json::json!({
            "total_cache_write_tokens": usage.total_cache_write_tokens,
        }));
    }
    metrics
}

/// Reverse of [`chat_usage_to_final_metrics`]: reconstruct a legacy
/// [`ChatUsage`] from ATIF's native `final_metrics`, reading
/// `total_cache_write_tokens` back out of `extra` if present.
pub fn final_metrics_to_chat_usage(metrics: &FinalMetrics) -> ChatUsage {
    let total_cache_write_tokens = metrics
        .extra
        .as_ref()
        .and_then(|v| v.get("total_cache_write_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    ChatUsage {
        total_input_tokens: metrics.total_prompt_tokens.unwrap_or(0),
        total_output_tokens: metrics.total_completion_tokens.unwrap_or(0),
        total_cache_read_tokens: metrics.total_cached_tokens.unwrap_or(0),
        total_cache_write_tokens,
        total_cost_usd: metrics.total_cost_usd.unwrap_or(0.0),
    }
}

// ── Session identity ─────────────────────────────────────────────────────────

/// Generate a new random session identifier: a v4 UUID string, the same
/// convention `chat_document::SessionId::new()` uses today. ATIF's
/// `Trajectory.session_id` (a plain `Option<String>`) is the canonical home
/// for this — no separate ID newtype is introduced here.
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ── Context-compaction structured details (extra.sven on a System step) ────

/// Structured details for a context-compaction event, preserved at
/// `step.extra.sven` alongside the Section VII `context_management`
/// convention object (which only carries `type`/`boundary`, not the
/// numeric before/after token counts or sven's strategy/turn fields).
///
/// Note: this reuses the `"sven"` extra-object key name at the *step*
/// level, distinct in scope from [`SvenSessionMeta::EXTRA_KEY`] at the
/// *trajectory root* level — both follow the same "namespace sven's bits
/// under `extra.sven`" convention, just at different nesting depths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionDetails {
    /// Approximate token count before compaction.
    pub tokens_before: usize,
    /// Approximate token count after compaction.
    pub tokens_after: usize,
    /// Which compaction strategy was used (structured/narrative/emergency).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Agentic loop round in which compaction fired (0 = pre-submit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
}

impl ContextCompactionDetails {
    /// Extract compaction details from a step's `extra.sven` object, if
    /// present and well-formed.
    pub fn from_step_extra(extra: &Value) -> Option<Self> {
        extra
            .get("sven")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

// ── Turn assembly: streaming builder ────────────────────────────────────────

/// Buffered state for the [`TraceStep`] currently being assembled from a
/// run of non-turn-boundary events (assistant text / tool calls / tool
/// results folding into one agent turn).
#[derive(Debug, Default)]
struct PendingAgentStep {
    reasoning_content: Option<String>,
    message_text: Option<String>,
    tool_calls: Vec<ToolInvocation>,
    observation_results: Vec<ObservationEntry>,
}

/// Streaming assembler that folds a flat stream of conversation events
/// (user messages, assistant text, tool calls, tool results, thinking
/// blocks, context-compaction markers) into turn-shaped [`TraceStep`]s.
///
/// ATIF's `StepObject` is turn-shaped, not event-shaped: one agent step
/// carries `reasoning_content` + `message` + `tool_calls` + `observation`
/// (the paired tool results) all together. This assembler implements the
/// merge rule sven's flat event stream needs:
///
/// - A user message always closes whatever agent step was pending and
///   starts a fresh `User` step (pushed immediately, never buffered).
/// - A `Thinking` event always closes whatever agent step was pending and
///   opens a *new* agent step, seeding its `reasoning_content` — a fresh
///   reasoning block signals a new LLM turn.
/// - Assistant text, tool calls, and tool results never open a *new*
///   step by themselves if one is already pending: they fold into
///   whichever agent step is currently open (creating one with no
///   `reasoning_content` if none was open yet). This is what makes
///   `thinking → tool_call → tool_result → assistant_text` collapse into
///   a single step even though the assistant text arrives *after* the
///   tool result.
/// - `ContextCompacted` always closes whatever agent step was pending and
///   is pushed immediately as its own `System` step.
/// - [`Self::finish`] flushes any still-open agent step and returns the
///   complete step list.
///
/// `step_id`s are assigned sequentially starting at 1 as steps are closed.
#[derive(Debug, Default)]
pub struct StepAssembler {
    steps: Vec<TraceStep>,
    next_step_id: u64,
    pending: Option<PendingAgentStep>,
}

impl StepAssembler {
    /// Construct an empty assembler; the next step emitted will have
    /// `step_id == 1`.
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            next_step_id: 1,
            pending: None,
        }
    }

    /// Construct an empty assembler continuing an existing trajectory: the
    /// next step emitted will have `step_id == next_step_id`.
    ///
    /// Used when appending new turns to a trajectory that already has
    /// `steps` on disk (e.g. `--trace`/`--load-trace` continuing a prior
    /// run) — pass `existing_step_count + 1` so the combined step sequence
    /// stays contiguous starting at 1, as
    /// `validate_trajectory` (`sven-atif`) requires.
    pub fn resuming(next_step_id: u64) -> Self {
        Self {
            steps: Vec::new(),
            next_step_id,
            pending: None,
        }
    }

    /// Steps closed (pushed) so far, in order. Does **not** include any
    /// still-open pending agent step — call [`Self::finish`] to flush that.
    ///
    /// Useful for a real-time streaming consumer that wants to emit each step
    /// as soon as it is known complete, without waiting for the whole
    /// conversation to end: compare `closed_steps().len()` against a
    /// previously observed length to find newly-closed steps.
    pub fn closed_steps(&self) -> &[TraceStep] {
        &self.steps
    }

    /// Feed one [`Message`] into the assembler.
    ///
    /// `Role::System` messages are skipped (matching
    /// `chat_document::message_to_turn`'s existing convention: the agent
    /// regenerates its system prompt at runtime). Multimodal
    /// `MessageContent::ContentParts` messages are also dropped — this
    /// mirrors `chat_document.rs`'s own current behavior (its
    /// `message_to_turn` only handles `Text`/`ToolCall`/`ToolResult` and
    /// silently drops anything else), preserved here for parity rather than
    /// invented fresh; full multimodal fidelity (inline image data URLs vs.
    /// ATIF's file/path-based `ImageSourceSchema`) is a follow-up concern.
    pub fn push_message(&mut self, msg: &Message) {
        match (&msg.role, &msg.content) {
            (Role::System, _) => {}
            (Role::User, MessageContent::Text(text)) => {
                self.flush_pending();
                self.push_immediate_step(StepOrigin::User, MessageBody::text(text.clone()));
            }
            (Role::Assistant, MessageContent::Text(text)) => {
                let pending = self.pending_or_new();
                pending.message_text = Some(match pending.message_text.take() {
                    Some(prev) => format!("{prev}\n{text}"),
                    None => text.clone(),
                });
            }
            (
                Role::Assistant,
                MessageContent::ToolCall {
                    tool_call_id,
                    function,
                },
            ) => {
                let arguments = serde_json::from_str(&function.arguments)
                    .unwrap_or_else(|_| Value::Object(Map::new()));
                let invocation = ToolInvocation::new(tool_call_id.clone(), function.name.clone())
                    .with_arguments(arguments);
                self.pending_or_new().tool_calls.push(invocation);
            }
            (
                Role::Tool,
                MessageContent::ToolResult {
                    tool_call_id,
                    content,
                },
            ) => {
                let entry = ObservationEntry::for_call(tool_call_id.clone(), content.to_string());
                self.pending_or_new().observation_results.push(entry);
            }
            _ => {}
        }
    }

    /// Feed a thinking/reasoning block into the assembler. Always closes any
    /// currently-open agent step and starts a new one seeded with this
    /// reasoning content.
    pub fn push_thinking(&mut self, content: &str) {
        self.flush_pending();
        self.pending_or_new().reasoning_content = Some(content.to_string());
    }

    /// Feed a context-compaction marker into the assembler. Always closes
    /// any currently-open agent step and is pushed immediately as its own
    /// `System` step, with the Section VII `context_management` convention
    /// object (`type: "compaction"`, `boundary: "replace"`) plus the full
    /// structured [`ContextCompactionDetails`] both nested under `extra`.
    pub fn push_context_compacted(
        &mut self,
        tokens_before: usize,
        tokens_after: usize,
        strategy: Option<&str>,
        turn: Option<u32>,
    ) {
        self.flush_pending();
        let details = ContextCompactionDetails {
            tokens_before,
            tokens_after,
            strategy: strategy.map(String::from),
            turn,
        };
        let mut step = TraceStep::new(
            self.take_step_id(),
            StepOrigin::System,
            format!("context_compaction: {tokens_before} -> {tokens_after} tokens"),
        );
        ContextManagement::new("compaction", "replace").insert_into_extra(&mut step.extra);
        let extra = step.extra.get_or_insert_with(|| Value::Object(Map::new()));
        extra.as_object_mut().expect("just ensured object").insert(
            "sven".to_string(),
            serde_json::to_value(&details).expect("always serializes"),
        );
        self.steps.push(step);
    }

    /// Attach a completed subagent's embedded-trajectory reference to the
    /// *currently pending* agent step (creating one if none is open yet) as
    /// an additional observation result, carrying a [`SubagentRef`]
    /// (embedded form, resolved via `trajectory_id`) pointing at
    /// `child_trajectory_id`.
    ///
    /// # Why attach instead of closing the step
    ///
    /// The obvious-looking alternative — flush whatever's pending and push a
    /// standalone `System` marker step instead — is wrong for a *streaming*
    /// caller: the `task` tool call that
    /// spawned the subagent is itself part of the currently-pending step
    /// (its `ToolCallStarted` always arrives before the subagent's own
    /// completion signal), and that tool call's own result observation
    /// hasn't necessarily arrived yet either. Flushing here would split the
    /// tool call from its eventual tool-result observation across two
    /// different steps — which `atif::validate::validate_trajectory`'s
    /// `DanglingSourceCallId` rule rejects, since a `source_call_id` must
    /// resolve against a `tool_calls` entry in the *same* step. Attaching to
    /// the still-open pending step instead keeps everything about this turn
    /// (the tool call, its result, and the subagent it delegated to)
    /// together in one step, the same way a plain tool call's result does.
    /// (This was found by testing against a real `task`-tool run, not
    /// designed up front — see the CI runner's `event::finalize_subagent_child`.)
    ///
    /// `source_call_id`, when given, is the spawning tool call's id (e.g.
    /// the `task` call's `tool_call_id`) — passing it correlates this
    /// observation with that `tool_calls` entry the same way the tool's own
    /// result observation will.
    ///
    /// The caller is responsible for appending the completed child
    /// `Trajectory` itself to the eventual `Trajectory.subagent_trajectories`
    /// once one is assembled (see `sven_ci::runner::event`'s `SubagentEvent`
    /// handling).
    pub fn push_subagent_embedded(
        &mut self,
        source_call_id: Option<&str>,
        child_trajectory_id: &str,
        child_session_id: Option<&str>,
    ) {
        let entry = ObservationEntry {
            source_call_id: source_call_id.map(str::to_string),
            content: None,
            subagent_trajectory_ref: Some(vec![SubagentRef {
                trajectory_id: Some(child_trajectory_id.to_string()),
                trajectory_path: None,
                session_id: child_session_id.map(str::to_string),
                extra: None,
            }]),
            extra: None,
        };
        self.pending_or_new().observation_results.push(entry);
    }

    /// Flush any still-open agent step and return the complete, ordered
    /// list of assembled steps.
    pub fn finish(mut self) -> Vec<TraceStep> {
        self.flush_pending();
        self.steps
    }

    fn pending_or_new(&mut self) -> &mut PendingAgentStep {
        self.pending.get_or_insert_with(PendingAgentStep::default)
    }

    fn flush_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let step_id = self.take_step_id();
        self.steps.push(Self::pending_to_step(&pending, step_id));
    }

    /// Build the `TraceStep` a [`PendingAgentStep`] would become if flushed,
    /// without consuming it — the shared core of [`Self::flush_pending`] and
    /// [`Self::snapshot_including_pending`].
    fn pending_to_step(pending: &PendingAgentStep, step_id: u64) -> TraceStep {
        let mut step = TraceStep::new(
            step_id,
            StepOrigin::Agent,
            MessageBody::text(pending.message_text.clone().unwrap_or_default()),
        );
        step.reasoning_content = pending.reasoning_content.clone();
        step.tool_calls = if pending.tool_calls.is_empty() {
            None
        } else {
            Some(pending.tool_calls.clone())
        };
        step.observation = if pending.observation_results.is_empty() {
            None
        } else {
            Some(StepObservation {
                results: pending.observation_results.clone(),
            })
        };
        step
    }

    /// All closed steps plus a snapshot of any still-open pending agent step,
    /// without consuming the assembler or mutating its state (unlike
    /// [`Self::finish`]).
    ///
    /// Useful for an abrupt-exit flush (timeout, interrupt, fatal error) that
    /// wants to record interim progress — including a turn that was cut off
    /// mid-tool-call — before the process exits, mirroring the crash-
    /// survivability guarantee the old per-event JSONL flush provided.
    pub fn snapshot_including_pending(&self) -> Vec<TraceStep> {
        let mut steps = self.steps.clone();
        if let Some(pending) = &self.pending {
            steps.push(Self::pending_to_step(pending, self.next_step_id));
        }
        steps
    }

    fn push_immediate_step(&mut self, source: StepOrigin, message: MessageBody) {
        let step = TraceStep::new(self.take_step_id(), source, message);
        self.steps.push(step);
    }

    fn take_step_id(&mut self) -> u64 {
        let id = self.next_step_id;
        self.next_step_id += 1;
        id
    }
}

/// Batch convenience: assemble a `Vec<Message>` history into `Vec<TraceStep>`
/// in one call. Built directly on [`StepAssembler`]; since `Message` alone
/// carries no thinking/context-compaction events, this is equivalent to
/// feeding each message through [`StepAssembler::push_message`] in order.
pub fn messages_to_steps(messages: &[Message]) -> Vec<TraceStep> {
    let mut assembler = StepAssembler::new();
    for msg in messages {
        assembler.push_message(msg);
    }
    assembler.finish()
}

/// Batch convenience: assemble a full-fidelity `&[ConversationRecord]`
/// stream (messages, thinking blocks, context-compaction markers) into
/// `Vec<TraceStep>` in one call. Built directly on [`StepAssembler`].
pub fn conversation_records_to_steps(records: &[ConversationRecord]) -> Vec<TraceStep> {
    let mut assembler = StepAssembler::new();
    for record in records {
        push_conversation_record(&mut assembler, record);
    }
    assembler.finish()
}

fn push_conversation_record(assembler: &mut StepAssembler, record: &ConversationRecord) {
    match record {
        ConversationRecord::Message(msg) => assembler.push_message(msg),
        ConversationRecord::Thinking { content } => assembler.push_thinking(content),
        ConversationRecord::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn,
        } => assembler.push_context_compacted(
            *tokens_before,
            *tokens_after,
            strategy.as_deref(),
            *turn,
        ),
    }
}

// ── Copied-context preservation on the display→persist path ─────────────────
//
// The display-oriented views ([`steps_to_turn_records`],
// [`steps_to_conversation_records`], [`steps_to_messages`]) all OMIT steps
// with `is_copied_context == Some(true)`. Any caller that rebuilds a
// trajectory's steps from such a display view must therefore carry the
// omitted steps through separately — otherwise opening and re-saving a
// trajectory permanently deletes them (they are real steps; the flag only
// marks them as excluded from SFT export, per [`Trajectory::sft_steps`]).

/// The steps of `trajectory` that display views omit (copied context from a
/// continued trajectory), preserved verbatim for the rebuild path.
pub fn copied_context_steps(trajectory: &Trajectory) -> Vec<TraceStep> {
    trajectory
        .steps
        .iter()
        .filter(|s| s.is_excluded_from_sft())
        .cloned()
        .collect()
}

/// Renumber `copied` contiguously from 1 (spec: step ids are contiguous)
/// and return an assembler that continues after them.
fn seed_copied(copied: &[TraceStep]) -> (Vec<TraceStep>, StepAssembler) {
    let mut out: Vec<TraceStep> = copied.to_vec();
    for (i, step) in out.iter_mut().enumerate() {
        step.step_id = (i + 1) as u64;
    }
    let assembler = StepAssembler::resuming(out.len() as u64 + 1);
    (out, assembler)
}

/// Like [`conversation_records_to_steps`], but prepends `copied` (the steps
/// the display view omitted — see [`copied_context_steps`]) so a
/// display→persist round trip does not delete them. Fresh steps are numbered
/// after the copied prefix, keeping the sequence contiguous from 1.
pub fn conversation_records_to_steps_with_copied_context(
    copied: &[TraceStep],
    records: &[ConversationRecord],
) -> Vec<TraceStep> {
    if copied.is_empty() {
        return conversation_records_to_steps(records);
    }
    let (mut out, mut assembler) = seed_copied(copied);
    for record in records {
        push_conversation_record(&mut assembler, record);
    }
    out.extend(assembler.finish());
    out
}

/// Like [`turn_records_to_steps`], but prepends `copied` — the
/// [`TurnRecord`]-path twin of
/// [`conversation_records_to_steps_with_copied_context`].
pub fn turn_records_to_steps_with_copied_context(
    copied: &[TraceStep],
    turns: &[TurnRecord],
) -> Vec<TraceStep> {
    if copied.is_empty() {
        return turn_records_to_steps(turns);
    }
    let (mut out, mut assembler) = seed_copied(copied);
    for turn in turns {
        push_turn_record(&mut assembler, turn);
    }
    out.extend(assembler.finish());
    out
}

/// Batch convenience: assemble a legacy `&[TurnRecord]` list (from a YAML
/// `ChatDocument`) into `Vec<TraceStep>` in one call. Built directly on
/// [`StepAssembler`]; used by [`import_legacy_chat_document`].
pub fn turn_records_to_steps(turns: &[TurnRecord]) -> Vec<TraceStep> {
    let mut assembler = StepAssembler::new();
    for turn in turns {
        push_turn_record(&mut assembler, turn);
    }
    assembler.finish()
}

fn push_turn_record(assembler: &mut StepAssembler, turn: &TurnRecord) {
    match turn {
        TurnRecord::User { content } => assembler.push_message(&Message::user(content)),
        TurnRecord::Assistant { content } => assembler.push_message(&Message::assistant(content)),
        TurnRecord::Thinking { content } => assembler.push_thinking(content),
        TurnRecord::ToolCall {
            tool_call_id,
            name,
            arguments,
        } => {
            let args_json = crate::chat_document::yaml_to_json_str(arguments);
            assembler.push_message(&Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: tool_call_id.clone(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: args_json,
                    },
                },
            });
        }
        TurnRecord::ToolResult {
            tool_call_id,
            content,
        } => {
            assembler.push_message(&Message::tool_result(tool_call_id.clone(), content));
        }
        TurnRecord::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn,
        } => assembler.push_context_compacted(
            *tokens_before,
            *tokens_after,
            strategy.as_deref(),
            *turn,
        ),
    }
}

// ── Reverse: TraceStep ⇄ Message ────────────────────────────────────────────

/// Reverse of the turn assembler: un-merge `Vec<TraceStep>` back into a
/// `Vec<Message>` suitable for seeding an agent's history on resume
/// (equivalent to today's `chat_document::turns_to_messages`).
///
/// - `System`-source steps are skipped entirely.
/// - Steps with `is_copied_context == Some(true)` are skipped — per the
///   spec's SFT-filtering guidance ([`Trajectory::sft_steps`]), repurposed
///   here for the same underlying reason: don't replay content the model
///   didn't originally produce fresh.
/// - `reasoning_content` is **never** replayed back into a message ("thinking
///   not sent back to model", matching `turns_to_messages`'s documented
///   behavior).
/// - An agent step's `tool_calls` are emitted first (each as its own
///   `Role::Assistant` `ToolCall` message, in array order), then their
///   matching `observation.results` entries (each as its own `Role::Tool`
///   `ToolResult` message, matched by `tool_call_id == source_call_id`, in
///   the same order as `tool_calls`) — this is a canonical choice where the
///   original interleaving is ambiguous (multiple tool calls per step).
///   Finally, non-empty step `message` text is emitted as a trailing
///   `Role::Assistant` text message.
pub fn steps_to_messages(steps: &[TraceStep]) -> Vec<Message> {
    let mut out = Vec::new();
    for step in steps {
        if step.is_excluded_from_sft() {
            continue;
        }
        match step.source {
            StepOrigin::System => continue,
            StepOrigin::User => {
                if let Some(text) = message_body_to_text(&step.message) {
                    if !text.is_empty() {
                        out.push(Message::user(text));
                    }
                }
            }
            StepOrigin::Agent => {
                if let Some(tool_calls) = &step.tool_calls {
                    for call in tool_calls {
                        out.push(Message {
                            role: Role::Assistant,
                            content: MessageContent::ToolCall {
                                tool_call_id: call.tool_call_id.clone(),
                                function: FunctionCall {
                                    name: call.function_name.clone(),
                                    arguments: call.arguments.to_string(),
                                },
                            },
                        });
                    }
                    if let Some(observation) = &step.observation {
                        for call in tool_calls {
                            if let Some(entry) = observation.results.iter().find(|r| {
                                r.source_call_id.as_deref() == Some(call.tool_call_id.as_str())
                            }) {
                                let content = entry
                                    .content
                                    .as_ref()
                                    .and_then(message_body_to_text)
                                    .unwrap_or_default();
                                out.push(Message::tool_result(call.tool_call_id.clone(), content));
                            }
                        }
                    }
                }
                if let Some(text) = message_body_to_text(&step.message) {
                    if !text.is_empty() {
                        out.push(Message::assistant(text));
                    }
                }
            }
        }
    }
    out
}

/// Extract plain text from a [`MessageBody`]: the text form as-is, or (for
/// `Segments`) the concatenation of its text segments, dropping any image
/// segments — a best-effort, documented-lossy fallback since
/// `sven_model::Message` has no way to reference an ATIF file-path image
/// without reading it into a data URL first (out of scope here; see
/// [`StepAssembler::push_message`]'s doc comment for the forward-direction
/// counterpart of this same limitation).
fn message_body_to_text(body: &MessageBody) -> Option<String> {
    if let Some(text) = body.as_text() {
        return Some(text.to_string());
    }
    body.as_segments().map(|segments| {
        segments
            .iter()
            .filter_map(|segment| match segment {
                ContentSegment::Text { text } => Some(text.as_str()),
                ContentSegment::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

// ── Reverse: TraceStep ⇄ TurnRecord (legacy YAML) ───────────────────────────

/// Reverse of [`turn_records_to_steps`]: convert `Vec<TraceStep>` back into
/// legacy `Vec<TurnRecord>`, for callers that accumulate ATIF-native steps
/// during a run but still need to write (or update) a YAML `ChatDocument`
/// from them (e.g. `--output-chat` alongside `--trace`).
///
/// - Steps with `is_copied_context == Some(true)` are skipped, matching
///   [`steps_to_messages`]. **Callers that rebuild a trajectory from this
///   view must carry those steps through** (capture them with
///   [`copied_context_steps`] and rebuild with
///   [`turn_records_to_steps_with_copied_context`] /
///   [`conversation_records_to_steps_with_copied_context`]) or an
///   open→save round trip permanently deletes them.
/// - `System`-source steps are skipped, except a context-compaction step
///   (detected via [`ContextCompactionDetails::from_step_extra`]), which
///   becomes `TurnRecord::ContextCompacted`. Any other `System` step has no
///   `TurnRecord` analog and is dropped.
/// - An agent step's `reasoning_content`, if present, becomes a leading
///   `TurnRecord::Thinking`.
/// - `tool_calls` are emitted next (each its own `TurnRecord::ToolCall`, in
///   array order), matched by `tool_call_id` to their `observation.results`
///   entry (each its own `TurnRecord::ToolResult`) — same ordering rule as
///   [`steps_to_messages`]. Finally, non-empty step `message` text becomes a
///   trailing `TurnRecord::User`/`TurnRecord::Assistant` (by `source`).
pub fn steps_to_turn_records(steps: &[TraceStep]) -> Vec<TurnRecord> {
    let mut out = Vec::new();
    for step in steps {
        if step.is_excluded_from_sft() {
            continue;
        }
        match step.source {
            StepOrigin::System => {
                if let Some(extra) = &step.extra {
                    if let Some(details) = ContextCompactionDetails::from_step_extra(extra) {
                        out.push(TurnRecord::ContextCompacted {
                            tokens_before: details.tokens_before,
                            tokens_after: details.tokens_after,
                            strategy: details.strategy,
                            turn: details.turn,
                        });
                    }
                }
            }
            StepOrigin::User => {
                if let Some(text) = message_body_to_text(&step.message) {
                    if !text.is_empty() {
                        out.push(TurnRecord::User { content: text });
                    }
                }
            }
            StepOrigin::Agent => {
                if let Some(reasoning) = &step.reasoning_content {
                    out.push(TurnRecord::Thinking {
                        content: reasoning.clone(),
                    });
                }
                if let Some(tool_calls) = &step.tool_calls {
                    for call in tool_calls {
                        out.push(TurnRecord::ToolCall {
                            tool_call_id: call.tool_call_id.clone(),
                            name: call.function_name.clone(),
                            arguments: json_str_to_yaml(&call.arguments.to_string()),
                        });
                    }
                    if let Some(observation) = &step.observation {
                        for call in tool_calls {
                            if let Some(entry) = observation.results.iter().find(|r| {
                                r.source_call_id.as_deref() == Some(call.tool_call_id.as_str())
                            }) {
                                let content = entry
                                    .content
                                    .as_ref()
                                    .and_then(message_body_to_text)
                                    .unwrap_or_default();
                                out.push(TurnRecord::ToolResult {
                                    tool_call_id: call.tool_call_id.clone(),
                                    content,
                                });
                            }
                        }
                    }
                }
                if let Some(text) = message_body_to_text(&step.message) {
                    if !text.is_empty() {
                        out.push(TurnRecord::Assistant { content: text });
                    }
                }
            }
        }
    }
    out
}

/// Reverse of [`conversation_records_to_steps`]: convert `Vec<TraceStep>` back
/// into full-fidelity `Vec<ConversationRecord>` (messages, thinking blocks,
/// context-compaction markers), for callers that need to reconstruct a
/// display-oriented event stream from ATIF steps rather than the flat
/// `Message`-only view [`steps_to_messages`] provides.
///
/// Built directly on [`steps_to_turn_records`] + [`crate::chat_document::turns_to_records`]
/// — the same full-fidelity turn list, just re-expressed as `ConversationRecord`
/// instead of the legacy YAML-oriented `TurnRecord`. This keeps exactly one
/// TraceStep-to-turn-shape algorithm in the crate rather than duplicating the
/// step-walking logic for two output types.
pub fn steps_to_conversation_records(steps: &[TraceStep]) -> Vec<ConversationRecord> {
    let turns = steps_to_turn_records(steps);
    crate::chat_document::turns_to_records(&turns)
}

// ── File I/O built on atif::persist ────────────────────────────────────────

/// Directory sven stores ATIF trajectory session files in.
///
/// Defaults to `$XDG_DATA_HOME/sven/sessions` (i.e. `~/.local/share/sven/sessions`)
/// — deliberately a *new* directory, distinct from the legacy
/// `chat_document::chat_dir()` (`.../sven/chats`), so old `.yaml` files and
/// new `.json` trajectory files never collide in the same directory listing.
pub fn session_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        })
        .join("sven")
        .join("sessions")
}

/// Create the session directory if it does not exist and return its path.
pub fn ensure_session_dir() -> Result<PathBuf> {
    let dir = session_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating session directory {}", dir.display()))?;
    Ok(dir)
}

/// The canonical file path for a session, given its `session_id`.
pub fn session_path(session_id: &str) -> PathBuf {
    session_dir().join(format!("{session_id}.json"))
}

/// Load a trajectory from an explicit file path.
///
/// Callers that want to *write* a session go through
/// [`atif::persist::write_trajectory_atomic`] directly (see the TUI's
/// `save_history_async`).
pub fn load_session_from(path: &Path) -> Result<Trajectory> {
    let (trajectory, _fingerprint) = atif::persist::read_trajectory_with_fingerprint(path)?;
    Ok(trajectory)
}

/// Summary of a session shown when listing sessions, built from a cheap
/// header-only read (never deserializes the `steps` array).
#[derive(Debug, Clone)]
pub struct SessionEntry {
    /// `Trajectory.session_id`, or an empty string if absent (shouldn't
    /// happen for a file this module wrote).
    pub session_id: String,
    /// Full path to the session's `.json` file.
    pub path: PathBuf,
    /// sven's own metadata (title, status, ...), if present and well-formed.
    pub meta: Option<SvenSessionMeta>,
    /// Aggregate token/cost metrics, if present.
    pub final_metrics: Option<FinalMetrics>,
}

/// List all session files in [`session_dir`], most recently updated first
/// (falling back to filename order for entries with no `meta.updated_at`).
/// Uses [`atif::persist::read_trajectory_header`] so listing stays cheap
/// even with a huge `steps` array in any individual file.
pub fn list_sessions(limit: Option<usize>) -> Result<Vec<SessionEntry>> {
    let dir = session_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(&dir).context("reading session directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match atif::persist::read_trajectory_header(&path) {
            Ok(header) => {
                let meta = header
                    .extra
                    .as_ref()
                    .and_then(|extra| extra.get(SvenSessionMeta::EXTRA_KEY))
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                entries.push(SessionEntry {
                    session_id: header.session_id.clone().unwrap_or_default(),
                    path,
                    meta,
                    final_metrics: header.final_metrics,
                });
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping malformed session file");
            }
        }
    }

    entries.sort_by(|a, b| {
        let updated_a = a.meta.as_ref().map(|m| m.updated_at);
        let updated_b = b.meta.as_ref().map(|m| m.updated_at);
        updated_b.cmp(&updated_a)
    });
    if let Some(n) = limit {
        entries.truncate(n);
    }
    Ok(entries)
}

// ── Unified listing: new ATIF sessions + legacy YAML chats ─────────────────

/// One row in the unified session picker: either a native ATIF session
/// (`is_legacy == false`) or a legacy YAML chat that has not yet been
/// re-saved in the new format (`is_legacy == true`).
///
/// # Legacy visibility policy
///
/// A legacy `.yaml` chat is surfaced here **only until** a same-`session_id`
/// `.json` file exists in [`session_dir`] — once a legacy session is opened
/// and saved again (which always writes the new format; see
/// [`import_legacy_chat_document`]'s doc comment), its `.json` twin appears
/// in the new-format listing under the same id and this function stops
/// surfacing the old `.yaml` entry. The original `.yaml` file itself is
/// **never deleted or modified** by this crate — it is left on disk,
/// superseded, purely for manual archival/recovery. This keeps the picker
/// free of duplicate rows while never destroying old data.
#[derive(Debug, Clone)]
pub struct UnifiedSessionEntry {
    /// Session identifier (shared between the legacy and new-format file for
    /// the same session, since [`import_legacy_chat_document`] preserves it).
    pub session_id: String,
    /// Full path to the backing file (`.json` if `!is_legacy`, `.yaml` if `is_legacy`).
    pub path: PathBuf,
    /// Human-readable title.
    pub title: String,
    /// Session lifecycle status.
    pub status: ChatStatus,
    /// Parent session ID when this is a subagent task conversation.
    pub parent_session_id: Option<String>,
    /// Cumulative token usage and cost, if any.
    pub usage: Option<ChatUsage>,
    /// When this session was last updated.
    pub updated_at: DateTime<Utc>,
    /// `true` iff this entry is backed by a legacy YAML `ChatDocument` file
    /// with no new-format `.json` twin yet.
    pub is_legacy: bool,
}

/// List every known session — native ATIF sessions from [`session_dir`] plus
/// any legacy YAML chats from [`crate::chat_document::chat_dir`] that have not
/// yet been superseded by a same-id `.json` file — most recently updated
/// first. See [`UnifiedSessionEntry`]'s doc comment for the exact legacy
/// visibility policy.
///
/// Uses [`list_sessions`] (header-only reads) for the new-format half, so
/// listing stays cheap even with many/large session files.
pub fn list_all_sessions(limit: Option<usize>) -> Result<Vec<UnifiedSessionEntry>> {
    let native = list_sessions(None)?;
    let legacy = crate::chat_document::list_chats(None).unwrap_or_default();
    let mut out = merge_native_and_legacy(native, legacy);
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    if let Some(n) = limit {
        out.truncate(n);
    }
    Ok(out)
}

/// Pure merge/dedup step behind [`list_all_sessions`], factored out so it can
/// be unit-tested without touching the real [`session_dir`]/[`crate::chat_document::chat_dir`]
/// filesystem locations. Does not sort or truncate — callers do that.
fn merge_native_and_legacy(
    native: Vec<SessionEntry>,
    legacy: Vec<crate::chat_document::ChatEntry>,
) -> Vec<UnifiedSessionEntry> {
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<UnifiedSessionEntry> = Vec::with_capacity(native.len() + legacy.len());

    for entry in native {
        seen_ids.insert(entry.session_id.clone());
        let (title, status, parent_session_id, updated_at) = match &entry.meta {
            Some(meta) => (
                meta.title.clone(),
                meta.status,
                meta.parent_session_id.clone(),
                meta.updated_at,
            ),
            None => (
                "Untitled".to_string(),
                ChatStatus::default(),
                None,
                Utc::now(),
            ),
        };
        let usage = entry
            .final_metrics
            .as_ref()
            .map(final_metrics_to_chat_usage);
        out.push(UnifiedSessionEntry {
            session_id: entry.session_id,
            path: entry.path,
            title,
            status,
            parent_session_id,
            usage,
            updated_at,
            is_legacy: false,
        });
    }

    for chat in legacy {
        let id = chat.id.as_str().to_string();
        if seen_ids.contains(&id) {
            // Superseded by a new-format twin; do not surface the legacy row.
            continue;
        }
        out.push(UnifiedSessionEntry {
            session_id: id,
            path: chat.path,
            title: chat.title,
            status: chat.status,
            parent_session_id: chat.parent_id.map(|p| p.as_str().to_string()),
            usage: chat.usage,
            updated_at: chat.updated_at,
            is_legacy: true,
        });
    }

    out
}

// ── Legacy YAML importer ────────────────────────────────────────────────────

/// One-way importer: read an existing legacy [`ChatDocument`] (YAML format)
/// and produce an equivalent ATIF [`Trajectory`]. Read-only — there is no
/// writer back to the old format from this module.
///
/// Mapping applied:
/// - `id` → `session_id`.
/// - `model` → `agent.model_name`.
/// - `turns` → `steps`, via [`turn_records_to_steps`] (full-fidelity turn
///   assembly, including thinking and context-compaction turns).
/// - `usage` (when non-empty) → `final_metrics`, via
///   [`chat_usage_to_final_metrics`].
/// - `title`/`status`/`mode`/`parent_id`/`created_at`/`updated_at` →
///   `extra.sven`, via [`SvenSessionMeta`].
pub fn import_legacy_chat_document(doc: &ChatDocument) -> Trajectory {
    let mut agent = default_agent_profile();
    if let Some(model) = &doc.model {
        agent = agent.with_model(model.clone());
    }

    let mut trajectory = Trajectory::new(ATIF_SCHEMA_VERSION, agent);
    trajectory.session_id = Some(doc.id.as_str().to_string());
    trajectory.steps = turn_records_to_steps(&doc.turns);

    if let Some(usage) = &doc.usage {
        if !usage.is_empty() {
            trajectory.final_metrics = Some(chat_usage_to_final_metrics(usage));
        }
    }

    let meta = SvenSessionMeta {
        title: doc.title.clone(),
        status: doc.status,
        mode: doc.mode.clone(),
        parent_session_id: doc.parent_id.as_ref().map(|p| p.as_str().to_string()),
        created_at: doc.created_at,
        updated_at: doc.updated_at,
    };
    meta.apply_to_trajectory(&mut trajectory);

    trajectory
}

/// Outcome of a [`migrate_legacy_chats`] run: the session id of every chat in
/// each of the three buckets.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationSummary {
    /// Converted and written out this run (or, under `dry_run`, would be).
    pub migrated: Vec<String>,
    /// Already had a `.json` trajectory at the target path — left untouched.
    pub skipped_existing: Vec<String>,
    /// `(session_id, error message)` for chats that failed to read or convert.
    pub failed: Vec<(String, String)>,
}

/// Bulk one-shot migration: every legacy `.yaml` chat in [`chat_document::chat_dir`]
/// that doesn't already have a corresponding ATIF `.json` file in
/// [`session_dir`] is converted via [`import_legacy_chat_document`] and
/// written out.
///
/// Idempotent — an existing `.json` for a session id is left untouched, never
/// overwritten (lazy per-open import already produces exactly one write per
/// session the first time it's opened; this just runs that same conversion
/// up front for every legacy chat at once instead of one at a time). The
/// original `.yaml` file is never modified or deleted.
///
/// `dry_run` reports what would happen (same [`MigrationSummary`] shape)
/// without writing anything or creating [`session_dir`].
pub fn migrate_legacy_chats(dry_run: bool) -> Result<MigrationSummary> {
    migrate_legacy_chats_between(&chat_document::chat_dir(), &session_dir(), dry_run)
}

/// [`migrate_legacy_chats`]'s implementation, parameterized on the source
/// (legacy `.yaml`) and target (ATIF `.json`) directories so it's testable
/// without touching the real `$XDG_DATA_HOME`.
fn migrate_legacy_chats_between(
    chat_dir: &Path,
    session_dir: &Path,
    dry_run: bool,
) -> Result<MigrationSummary> {
    let mut summary = MigrationSummary::default();
    if !chat_dir.exists() {
        return Ok(summary);
    }
    if !dry_run {
        fs::create_dir_all(session_dir)
            .with_context(|| format!("creating session directory {}", session_dir.display()))?;
    }
    for entry in
        fs::read_dir(chat_dir).with_context(|| format!("reading {}", chat_dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let target = session_dir.join(format!("{id}.json"));
        if target.exists() {
            summary.skipped_existing.push(id);
            continue;
        }
        if dry_run {
            summary.migrated.push(id);
            continue;
        }
        match chat_document::load_chat_from(&path) {
            Ok(doc) => {
                let trajectory = import_legacy_chat_document(&doc);
                match atif::persist::write_trajectory(&target, &trajectory) {
                    Ok(()) => summary.migrated.push(id),
                    Err(e) => summary.failed.push((id, e.to_string())),
                }
            }
            Err(e) => summary.failed.push((id, e.to_string())),
        }
    }
    summary.migrated.sort();
    summary.skipped_existing.sort();
    summary.failed.sort();
    Ok(summary)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "trace_session_tests.rs"]
mod tests;
