// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Append-only per-thread conversation storage.
//!
//! A machine keeps one conversation **thread** per state (e.g. `intake`,
//! `discovery`, `task:t1`). Each thread is an append-only `Vec<Message>`:
//! turns are only ever pushed, never rewritten. This is the cache-safety
//! invariant - the provider's prompt cache stays valid because the prefix
//! never changes. Cross-state / cross-submachine context is carried by
//! *appending a new user turn* to the destination thread, never by editing
//! history.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use atif::{CallTiming, GateAuthority, GateDecision, GateOutcome, MemoryRetrieval};
use sven_hsm::{ProposedToolCall, ToolCallId, ToolCapability};
use sven_tool_registry::ToolRegistry;

use sven_model::{Message, MessageContent, Role};
use sven_vocab::{CompactionRecord, RequestRecord};

/// Stable string identifier for a conversation thread (e.g. `"intake"`).
pub type ThreadId = String;

/// Owns one append-only `Vec<Message>` per thread for the lifetime of a runtime.
///
/// Threads are created lazily on first access.  The store never rewrites or
/// removes earlier turns — only [`append`](ThreadStore::append) is
/// exposed for mutation, preserving the cache-safety invariant.
#[derive(Debug, Default)]
pub struct ThreadStore {
    threads: HashMap<ThreadId, Vec<Message>>,
    /// Why the caller is stopping the run, set just before it cancels the
    /// turn in flight; the turn executor labels the aborted turn with it.
    stop_reason: Option<String>,
    /// The agent's todo list, kept here (outside the conversation) so a
    /// compaction can carry it through verbatim.
    todos: Vec<sven_vocab::TodoItem>,
    /// The model's raw argument text, by call id, for tool calls whose
    /// arguments were not valid JSON. The thread holds such a call with `{}`
    /// arguments - history is replayed to providers, and a malformed string
    /// there would be rejected by some - so this is where the original text
    /// is kept for whoever records the run.
    unparsed_arguments: HashMap<String, String>,
    /// Every compaction applied to any thread, oldest first.
    compactions: Vec<CompactionRecord>,
    /// What the permission gate decided about a call, by the model's call id.
    /// Kept here, beside the thread that holds the call, so whoever records
    /// the run finds the decision and the call together.
    gate_decisions: HashMap<String, GateDecision>,
    /// What each memory retrieval a tool made returned, oldest first.
    memory_retrievals: Vec<MemoryRetrieval>,
    /// How long each tool call ran, by the model's call id.
    call_timings: HashMap<String, CallTiming>,
    /// Every message ever appended, in order, with its thread. Unlike the
    /// threads, which a compaction rewrites to what the model reads next, this
    /// is never rewritten: it is the record of what happened, which a
    /// trajectory export reads (see [`Self::transcript`]).
    log: Vec<(ThreadId, Message)>,
    /// Where a user turn came from, by thread and transcript position: the
    /// instruction a harness step sent (`task`, `follow_up`, `repair`,
    /// `correction`).
    origins: Vec<(ThreadId, usize, String)>,
    /// Reasoning of turns that produced no message (cut off while thinking),
    /// with the transcript position the next message will take. A turn that
    /// produced a message carries its reasoning on that message instead.
    unspoken_reasonings: Vec<(ThreadId, usize, String)>,
    /// The thread's transcript length at each entry of `compactions`.
    compaction_positions: Vec<usize>,
    /// Each model request's account, with the thread and the transcript
    /// length when its response was complete (before the response's own
    /// messages were appended).
    requests: Vec<(ThreadId, usize, RequestRecord)>,
}

impl ThreadStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` if the named thread has no turns yet (or does not exist).
    #[must_use]
    pub fn is_empty(&self, id: &str) -> bool {
        self.threads.get(id).map(Vec::is_empty).unwrap_or(true)
    }

    /// `true` if the thread exists (even if it has been created but is empty).
    #[must_use]
    pub fn exists(&self, id: &str) -> bool {
        self.threads.contains_key(id)
    }

    /// Mutable access to a thread's message buffer, creating it if absent.
    ///
    /// Callers must only ever *push* onto the returned buffer; mutating earlier
    /// turns breaks the prompt-cache invariant.
    pub fn thread(&mut self, id: &str) -> &mut Vec<Message> {
        self.threads.entry(id.to_string()).or_default()
    }

    /// Replaces the stored todo list with the tool's latest.
    pub fn set_todos(&mut self, todos: Vec<sven_vocab::TodoItem>) {
        self.todos = todos;
    }

    /// The todo list as the tool last reported it.
    #[must_use]
    pub fn todos(&self) -> &[sven_vocab::TodoItem] {
        &self.todos
    }

    /// Notes why the run is being stopped from outside; read once by
    /// [`Self::take_stop_reason`] when the aborted turn is filed.
    pub fn set_stop_reason(&mut self, reason: &str) {
        self.stop_reason = Some(reason.to_string());
    }

    /// Takes the reason given by [`Self::set_stop_reason`].
    pub fn take_stop_reason(&mut self) -> Option<String> {
        self.stop_reason.take()
    }

    /// Starts an empty thread with `system` as its system message. A thread
    /// that already has turns keeps its own start.
    pub fn open_thread(&mut self, id: &str, system: Option<&str>) {
        if let (Some(text), true) = (system, self.is_empty(id)) {
            self.append(id, Message::system(text));
        }
    }

    /// Appends a user turn and files where it came from.
    pub fn append_labeled(&mut self, id: &str, message: Message, origin: &str) {
        let position = self.transcript_len(id);
        self.origins
            .push((id.to_string(), position, origin.to_string()));
        self.append(id, message);
    }

    /// The origins filed for `id`, each with its position in the transcript.
    #[must_use]
    pub fn transcript_origins(&self, id: &str) -> Vec<(usize, String)> {
        self.origins
            .iter()
            .filter(|(thread, _, _)| thread == id)
            .map(|(_, position, origin)| (*position, origin.clone()))
            .collect()
    }

    /// Files the reasoning of a turn that produced no message to carry it.
    pub fn record_unspoken_reasoning(&mut self, id: &str, reasoning: String) {
        if reasoning.is_empty() {
            return;
        }
        let position = self.transcript_len(id);
        self.unspoken_reasonings
            .push((id.to_string(), position, reasoning));
    }

    /// The reasoning of each assistant turn of `id`, in order, with the
    /// transcript position of the message it preceded: the reasoning a turn's
    /// first message carries, and that of turns that produced no message.
    #[must_use]
    pub fn transcript_reasonings(&self, id: &str) -> Vec<(usize, String)> {
        // Unspoken turns first: at an equal position the stable sort keeps
        // them ahead of the message a later turn produced there.
        let mut out: Vec<(usize, String)> = self
            .unspoken_reasonings
            .iter()
            .filter(|(thread, _, _)| thread == id)
            .map(|(_, position, text)| (*position, text.clone()))
            .collect();
        out.extend(
            self.log
                .iter()
                .filter(|(thread, _)| thread == id)
                .enumerate()
                .filter_map(|(position, (_, m))| m.reasoning.clone().map(|r| (position, r))),
        );
        out.sort_by_key(|(position, _)| *position);
        out
    }

    /// Append a single turn to a thread (creating the thread if needed).
    pub fn append(&mut self, id: &str, message: Message) {
        self.log.push((id.to_string(), message.clone()));
        self.threads
            .entry(id.to_string())
            .or_default()
            .push(message);
    }

    /// Replace a thread's entire contents with `messages`.
    ///
    /// Unlike [`append`](Self::append), this is **not** append-only: it is the
    /// history-seeding escape hatch used by the interactive frontends when the
    /// user edits and resubmits an earlier turn (edit-resubmit) or resumes a
    /// saved session. The frontend reconstructs the authoritative history and
    /// installs it here so the next turn streams against exactly those turns,
    /// not the store's own accumulated version. `TurnExecutor` also uses it to
    /// install a compacted history. Because the prefix changes, the provider
    /// prompt cache for this thread is intentionally invalidated.
    pub fn replace_thread(&mut self, id: &str, messages: Vec<Message>) {
        self.threads.insert(id.to_string(), messages);
    }

    /// A read-only clone of a thread's current turns (empty if absent).
    #[must_use]
    pub fn snapshot(&self, id: &str) -> Vec<Message> {
        self.threads.get(id).cloned().unwrap_or_default()
    }

    /// Answers every tool call in `id` that has no result yet, so the thread
    /// is a history a provider accepts. `reasons` maps the model's call id to
    /// why the call was not run; a call without one is answered generically.
    /// Returns how many calls were answered.
    pub fn answer_open_calls(&mut self, id: &str, answers: &HashMap<String, String>) -> usize {
        let open: Vec<String> = self
            .open_calls(id)
            .into_iter()
            .map(|(call, _)| call.to_string())
            .collect();
        for call in &open {
            let answer = answers
                .get(call)
                .cloned()
                .unwrap_or_else(|| "The call was not run: it was refused.".to_string());
            self.append(id, Message::tool_result(call, answer));
        }
        open.len()
    }

    /// The calls in thread `id` that no result answers, as the model made
    /// them: its id for the call and the function it named.
    fn open_calls(&self, id: &str) -> Vec<(&str, &sven_model::FunctionCall)> {
        let Some(thread) = self.threads.get(id) else {
            return Vec::new();
        };
        let answered: std::collections::HashSet<&str> = thread
            .iter()
            .filter_map(|m| match &m.content {
                MessageContent::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        thread
            .iter()
            .filter_map(|m| match &m.content {
                MessageContent::ToolCall {
                    tool_call_id,
                    function,
                } if !answered.contains(tool_call_id.as_str()) => {
                    Some((tool_call_id.as_str(), function))
                }
                _ => None,
            })
            .collect()
    }

    /// The registry entries (`kernel id -> (thread, model id)`) for the calls
    /// of thread `id` that no result answers, recovered after the session
    /// that proposed them is gone.
    ///
    /// The kernel names a call by an id it minted, and only the registry
    /// knows the model's id for it; a machine's record of `proposed` calls
    /// outlives the session, so each is paired with the open call of the same
    /// function and arguments. Two identical calls pair in either order,
    /// which is indistinguishable to everyone.
    #[must_use]
    pub fn recover_open_calls<'a>(
        &self,
        id: &str,
        proposed: impl IntoIterator<Item = &'a ProposedToolCall>,
    ) -> HashMap<ToolCallId, (ThreadId, String)> {
        let mut open = self.open_calls(id);
        let mut recovered = HashMap::new();
        for call in proposed {
            let found = open.iter().position(|(_, function)| {
                function.name == call.name
                    && serde_json::from_str::<serde_json::Value>(&function.arguments)
                        .is_ok_and(|args| args == call.args)
            });
            if let Some(found) = found {
                let (model_id, _) = open.swap_remove(found);
                recovered.insert(call.call_id, (id.to_string(), model_id.to_string()));
            }
        }
        recovered
    }

    /// Keeps the raw argument text of tool calls the model garbled beyond
    /// repair, by call id. See [`unparsed_arguments`](Self::unparsed_arguments).
    pub fn record_unparsed_arguments(&mut self, raw: HashMap<String, String>) {
        self.unparsed_arguments.extend(raw);
    }

    /// Raw argument text, by call id, of every call recorded with
    /// [`record_unparsed_arguments`](Self::record_unparsed_arguments).
    #[must_use]
    pub fn unparsed_arguments(&self) -> &HashMap<String, String> {
        &self.unparsed_arguments
    }

    /// Keeps the gate's decision about the call the model named `call_id`.
    ///
    /// Argument violations already noted for the call stay with it unless
    /// `decision` brings its own.
    pub fn record_gate_decision(&mut self, call_id: &str, mut decision: GateDecision) {
        if decision.violations.is_empty() {
            if let Some(known) = self.gate_decisions.remove(call_id) {
                decision.violations = known.violations;
            }
        }
        self.gate_decisions.insert(call_id.to_string(), decision);
    }

    /// Keeps the `violations` a schema pre-check found in the arguments of the
    /// call the model named `call_id`. A call the gate said nothing about is
    /// filed as `unasked`: the policy let it through without asking anyone.
    pub fn record_argument_violations(
        &mut self,
        call_id: &str,
        unasked: impl FnOnce() -> GateDecision,
        violations: Vec<atif::ArgumentViolation>,
    ) {
        self.gate_decisions
            .entry(call_id.to_string())
            .or_insert_with(unasked)
            .violations = violations;
    }

    /// Every gate decision recorded so far, ordered by call id.
    #[must_use]
    pub fn gate_decisions(&self) -> Vec<GateDecision> {
        let mut decisions: Vec<_> = self.gate_decisions.values().cloned().collect();
        decisions.sort_by(|a, b| a.call_id.cmp(&b.call_id));
        decisions
    }

    /// Keeps what a memory retrieval returned.
    pub fn record_memory_retrieval(&mut self, retrieval: MemoryRetrieval) {
        self.memory_retrievals.push(retrieval);
    }

    /// Every memory retrieval recorded so far, oldest first.
    #[must_use]
    pub fn memory_retrievals(&self) -> &[MemoryRetrieval] {
        &self.memory_retrievals
    }

    /// Keeps how long the tool call the model named `call_id` ran, replacing
    /// an earlier figure for the same call.
    pub fn record_call_timing(&mut self, call_id: &str, duration: std::time::Duration) {
        self.call_timings.insert(
            call_id.to_string(),
            CallTiming {
                call_id: call_id.to_string(),
                duration_ms: duration.as_secs_f64() * 1000.0,
            },
        );
    }

    /// Every call duration recorded so far, ordered by call id.
    #[must_use]
    pub fn call_timings(&self) -> Vec<CallTiming> {
        let mut timings: Vec<_> = self.call_timings.values().cloned().collect();
        timings.sort_by(|a, b| a.call_id.cmp(&b.call_id));
        timings
    }

    /// Notes that a thread was compacted. Call alongside
    /// [`replace_thread`](Self::replace_thread) with the compacted history.
    pub fn record_compaction(&mut self, record: CompactionRecord) {
        let position = self.transcript_len(&record.thread);
        self.compaction_positions.push(position);
        self.compactions.push(record);
    }

    /// Every message ever appended to `id`, in order, whatever a compaction
    /// later did to the thread. Messages installed with
    /// [`replace_thread`](Self::replace_thread) (seeding, compaction) are not
    /// in it: they are not new events.
    #[must_use]
    pub fn transcript(&self, id: &str) -> Vec<Message> {
        self.log
            .iter()
            .filter(|(thread, _)| thread == id)
            .map(|(_, message)| message.clone())
            .collect()
    }

    /// The threads that have a transcript, in the order each first received a
    /// message.
    #[must_use]
    pub fn transcript_threads(&self) -> Vec<ThreadId> {
        let mut seen = Vec::new();
        for (thread, _) in &self.log {
            if !seen.contains(thread) {
                seen.push(thread.clone());
            }
        }
        seen
    }

    fn transcript_len(&self, id: &str) -> usize {
        self.log.iter().filter(|(thread, _)| thread == id).count()
    }

    /// The compactions of `id` and where in its [`transcript`](Self::transcript)
    /// each fired, oldest first.
    #[must_use]
    pub fn transcript_compactions(&self, id: &str) -> Vec<(usize, CompactionRecord)> {
        self.compactions
            .iter()
            .zip(&self.compaction_positions)
            .filter(|(record, _)| record.thread == id)
            .map(|(record, position)| (*position, record.clone()))
            .collect()
    }

    /// Keeps the account of one model request on `id`. Call when the response
    /// is complete and before its messages are appended.
    pub fn record_request(&mut self, id: &str, request: RequestRecord) {
        let before = self.transcript_len(id);
        self.requests.push((id.to_string(), before, request));
    }

    /// The requests made on `id`, oldest first, each with the number of
    /// transcript messages up to and including its response's: the run of
    /// assistant messages that follows where the response began.
    #[must_use]
    pub fn transcript_requests(&self, id: &str) -> Vec<(usize, RequestRecord)> {
        let transcript = self.transcript(id);
        self.requests
            .iter()
            .filter(|(thread, _, _)| thread == id)
            .map(|(_, before, request)| {
                let answered = transcript
                    .iter()
                    .skip(*before)
                    .take_while(|m| m.role == Role::Assistant)
                    .count();
                (before + answered, request.clone())
            })
            .collect()
    }

    /// Every compaction recorded so far, oldest first.
    #[must_use]
    pub fn compactions(&self) -> &[CompactionRecord] {
        &self.compactions
    }

    /// Number of turns currently stored in a thread.
    #[must_use]
    pub fn len(&self, id: &str) -> usize {
        self.threads.get(id).map(Vec::len).unwrap_or(0)
    }
}

/// The `policy` a decision is filed under when the policy required a person's
/// approval of the call.
pub(crate) const REQUIRES_APPROVAL: &str = "requires_approval";

/// Files gate decisions in the conversation store under the model's id for the
/// call, which is the id a trajectory names it by.
///
/// The gate, the person asked and the executor that runs the tool each know a
/// different part of what happened to a call, and each knows the call by the
/// kernel's id. The journal holds the registry that translates, so none of
/// them needs the store or the model's ids.
#[derive(Clone)]
pub struct GateJournal {
    store: Arc<Mutex<ThreadStore>>,
    registry: Arc<Mutex<HashMap<ToolCallId, (String, String)>>>,
}

/// What a party to a gate decision reports to the [`GateJournal`].
pub struct GateVerdict<'a> {
    /// The capability the call needed.
    pub capability: ToolCapability,
    /// The rule that routed the call; see [`GateDecision::policy`].
    pub policy: &'a str,
    /// What was decided.
    pub outcome: GateOutcome,
    /// Who decided it.
    pub decided_by: GateAuthority,
    /// Why, when whoever decided said.
    pub reason: Option<String>,
}

impl GateVerdict<'static> {
    /// A person's decision about a call that policy asked them to approve.
    #[must_use]
    pub fn human(capability: ToolCapability, approved: bool, reason: Option<String>) -> Self {
        Self {
            capability,
            policy: REQUIRES_APPROVAL,
            outcome: if approved {
                GateOutcome::Approved
            } else {
                GateOutcome::Denied
            },
            decided_by: GateAuthority::Human,
            reason,
        }
    }
}

impl GateJournal {
    /// A journal writing to `store`, translating ids with `registry`.
    #[must_use]
    pub fn new(
        store: Arc<Mutex<ThreadStore>>,
        registry: Arc<Mutex<HashMap<ToolCallId, (String, String)>>>,
    ) -> Self {
        Self { store, registry }
    }

    /// Records `verdict` for the call the kernel names `call_id`. A call the
    /// registry does not know is not part of any thread, so there is no
    /// trajectory step to attach a decision to and nothing is recorded.
    pub fn record(&self, call_id: ToolCallId, verdict: GateVerdict<'_>) {
        let Some(model_id) = self
            .registry
            .lock()
            .ok()
            .and_then(|registry| registry.get(&call_id).map(|(_, model_id)| model_id.clone()))
        else {
            return;
        };
        let decision = GateDecision {
            decision_id: call_id.as_uuid().to_string(),
            call_id: model_id.clone(),
            capability: format!("{:?}", verdict.capability),
            policy: verdict.policy.to_string(),
            outcome: verdict.outcome,
            decided_by: verdict.decided_by,
            reason: verdict.reason,
            violations: Vec::new(),
        };
        if let Ok(mut store) = self.store.lock() {
            store.record_gate_decision(&model_id, decision);
        }
    }
}

/// The model's own call id for each refused call, mapped to its reason:
/// the machine names calls by the kernel's id, the thread by the model's.
pub(crate) fn machine_answers(
    registry: &Mutex<HashMap<ToolCallId, (String, String)>>,
    answered: &[sven_vocab::AnsweredCall],
) -> HashMap<String, String> {
    let Ok(registry) = registry.lock() else {
        return HashMap::new();
    };
    registry
        .iter()
        .filter_map(|(id, (_, model_id))| {
            let id = id.as_uuid().to_string();
            let answer = answered.iter().find(|r| r.call_id == id)?;
            Some((model_id.clone(), answer.content.clone()))
        })
        .collect()
}

/// Register call_id → (thread, original_id) in the registry (non-async).
///
/// The `original_id` is the raw tool_call_id string returned by the LLM (e.g.
/// `"call_HyZJn1bTtqVbmzzS3W4VM4xb"` for OpenAI).  It is preserved so that
/// `ToolExecutor` can append tool results with the *same* id that appears in the
/// assistant message — the API rejects mismatches.
pub(crate) fn register_calls(
    registry: &Mutex<HashMap<ToolCallId, (String, String)>>,
    thread_id: &str,
    calls: &[sven_tool_api::ToolCall],
    tools: &ToolRegistry,
) -> Vec<ProposedToolCall> {
    let mut proposed = Vec::with_capacity(calls.len());
    if let Ok(mut reg) = registry.lock() {
        for tc in calls {
            let capability = tools.capability_of_call(tc);
            let call_id = ToolCallId::from_str_lossy(&tc.id);
            reg.insert(call_id, (thread_id.to_string(), tc.id.clone()));
            proposed.push(ProposedToolCall {
                call_id,
                name: tc.name.clone(),
                args: tc.args.clone(),
                capability,
            });
        }
    }
    proposed
}

/// Snapshot a thread from the store (non-async; guard is never held across await).
pub(crate) fn snapshot_thread(store: &Mutex<ThreadStore>, thread_id: &str) -> Vec<Message> {
    store
        .lock()
        .map(|s| s.snapshot(thread_id))
        .unwrap_or_default()
}

/// Append a batch of messages to a store thread (non-async).
pub(crate) fn append_messages(store: &Mutex<ThreadStore>, thread_id: &str, messages: Vec<Message>) {
    if let Ok(mut s) = store.lock() {
        for msg in messages {
            s.append(thread_id, msg);
        }
    }
}

/// The conversation a controller hands a session that does not run it yet,
/// as the session's own messages: the remote-control protocol's flat turns
/// (`sven_control::HistoryEntry`), oldest first. A tool turn is dropped: it is
/// only meaningful next to the call that made it, which a flat history cannot
/// carry, and the answer that used it says enough.
#[must_use]
pub fn history_messages(
    entries: impl IntoIterator<Item = sven_control::HistoryEntry>,
) -> Vec<Message> {
    use sven_control::HistoryRole;
    entries
        .into_iter()
        .filter_map(|entry| match entry.role {
            HistoryRole::User => Some(Message::user(entry.text)),
            HistoryRole::Assistant => Some(Message::assistant(entry.text)),
            HistoryRole::Tool | HistoryRole::System => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_flat_history_becomes_the_turns_a_session_can_continue() {
        use sven_control::{HistoryEntry, HistoryRole};
        let messages = history_messages([
            HistoryEntry {
                role: HistoryRole::User,
                text: "hi".into(),
            },
            HistoryEntry {
                role: HistoryRole::Tool,
                text: "ls output".into(),
            },
            HistoryEntry {
                role: HistoryRole::Assistant,
                text: "hello".into(),
            },
        ]);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].as_text(), Some("hi"));
        assert_eq!(messages[1].as_text(), Some("hello"));
    }

    use super::*;

    /// A compaction rewrites a thread to what the model reads next; the
    /// transcript is what happened, including the answer the store itself gave
    /// to a call that was never run.
    #[test]
    fn the_transcript_keeps_what_a_compaction_replaced_and_the_answers_to_open_calls() {
        let mut store = ThreadStore::new();
        store.append("t", Message::user("one"));
        store.append("t", Message::assistant("two"));
        store.replace_thread("t", vec![Message::assistant("a summary")]);
        store.record_compaction(CompactionRecord {
            thread: "t".into(),
            tokens_before: 100,
            tokens_after: 10,
            strategy: sven_hsm::CompactionStrategyUsed::Emergency,
            turn: 0,
        });
        store.append(
            "t",
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "c1".into(),
                    function: sven_model::FunctionCall {
                        name: "x".into(),
                        arguments: "{}".into(),
                    },
                },
                reasoning: None,
            },
        );
        store.answer_open_calls("t", &HashMap::new());

        let transcript = store.transcript("t");
        assert_eq!(transcript.len(), 4, "{transcript:?}");
        assert_eq!(transcript[0].as_text(), Some("one"));
        assert!(matches!(
            &transcript[3].content,
            MessageContent::ToolResult { tool_call_id, .. } if tool_call_id == "c1"
        ));
        assert_eq!(
            store.snapshot("t").len(),
            3,
            "the thread is the compacted view"
        );
        let marks = store.transcript_compactions("t");
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].0, 2, "the compaction fired after two messages");
    }

    #[test]
    fn store_is_append_only_and_snapshots() {
        let mut store = ThreadStore::new();
        assert!(store.is_empty("intake"));
        store.append("intake", Message::system("role"));
        store.append("intake", Message::user("hi"));
        assert_eq!(store.len("intake"), 2);
        let snap = store.snapshot("intake");
        assert_eq!(snap.len(), 2);
        // Snapshot is a clone; mutating it does not affect the store.
        assert!(!store.is_empty("intake"));
    }

    /// A call with no result is answered with its reason; one that has a
    /// result is left alone.
    #[test]
    fn open_calls_are_answered_once() {
        let call = |id: &str| Message {
            role: sven_model::Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: id.into(),
                function: sven_model::FunctionCall {
                    name: "deploy".into(),
                    arguments: "{}".into(),
                },
            },
            reasoning: None,
        };
        let mut store = ThreadStore::new();
        store.append("t", call("ran"));
        store.append("t", Message::tool_result("ran", "ok"));
        store.append("t", call("refused"));
        let reasons = HashMap::from([("refused".to_string(), "a human said no".to_string())]);
        assert_eq!(store.answer_open_calls("t", &reasons), 1);
        assert_eq!(store.answer_open_calls("t", &reasons), 0);
        let last = format!("{:?}", store.snapshot("t").last().unwrap());
        assert!(last.contains("a human said no"), "{last}");
    }

    fn decision(call_id: &str, by: GateAuthority) -> GateDecision {
        GateDecision {
            decision_id: format!("d-{call_id}"),
            call_id: call_id.into(),
            capability: "ExecuteShell".into(),
            policy: "requires_approval".into(),
            outcome: GateOutcome::Approved,
            decided_by: by,
            reason: None,
            violations: Vec::new(),
        }
    }

    fn violation() -> atif::ArgumentViolation {
        atif::ArgumentViolation {
            path: String::new(),
            rule: "required".into(),
            message: "missing".into(),
        }
    }

    /// Violations are noted when the call runs, after the decision that let it
    /// run: the decision must keep them, and a call nobody was asked about
    /// still gets a record to carry them.
    #[test]
    fn violations_join_the_decision_whichever_arrives_first() {
        let mut store = ThreadStore::new();
        store.record_gate_decision("a", decision("a", GateAuthority::Human));
        store.record_argument_violations(
            "a",
            || decision("a", GateAuthority::Policy),
            vec![violation()],
        );
        store.record_argument_violations(
            "b",
            || decision("b", GateAuthority::Policy),
            vec![violation()],
        );
        store.record_gate_decision("b", decision("b", GateAuthority::Human));

        let decisions = store.gate_decisions();
        assert_eq!(decisions.len(), 2);
        for d in &decisions {
            assert_eq!(d.decided_by, GateAuthority::Human, "{d:?}");
            assert_eq!(d.violations.len(), 1, "{d:?}");
        }
    }

    #[test]
    fn the_journal_files_a_decision_under_the_models_call_id() {
        let store = Arc::new(Mutex::new(ThreadStore::new()));
        let registry = Arc::new(Mutex::new(HashMap::new()));
        let kernel_id = ToolCallId::new();
        registry
            .lock()
            .unwrap()
            .insert(kernel_id, ("chat".to_string(), "call_xyz".to_string()));
        let journal = GateJournal::new(Arc::clone(&store), registry);

        journal.record(
            kernel_id,
            GateVerdict {
                capability: ToolCapability::ExecuteShell,
                policy: "requires_approval",
                outcome: GateOutcome::Denied,
                decided_by: GateAuthority::HandlerDropped,
                reason: Some("handler_dropped".into()),
            },
        );
        journal.record(
            ToolCallId::new(),
            GateVerdict {
                capability: ToolCapability::ExecuteShell,
                policy: "requires_approval",
                outcome: GateOutcome::Denied,
                decided_by: GateAuthority::Human,
                reason: None,
            },
        );

        let decisions = store.lock().unwrap().gate_decisions();
        assert_eq!(
            decisions.len(),
            1,
            "a call outside every thread is not recorded"
        );
        assert_eq!(decisions[0].call_id, "call_xyz");
        assert_eq!(decisions[0].decision_id, kernel_id.as_uuid().to_string());
        assert_eq!(decisions[0].capability, "ExecuteShell");
        assert_eq!(decisions[0].reason.as_deref(), Some("handler_dropped"));
    }

    #[test]
    fn thread_creates_lazily() {
        let mut store = ThreadStore::new();
        store.thread("discovery").push(Message::user("explore"));
        assert_eq!(store.len("discovery"), 1);
        assert!(store.exists("discovery"));
    }
}
