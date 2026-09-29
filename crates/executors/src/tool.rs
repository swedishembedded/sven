//! Tool effect executor.
//!
//! Handles [`Effect::CallTool`]: performs a second-line capability check,
//! spawns the tool execution on a dedicated tokio task (spawn-and-forget so a
//! batch of `CallTool` effects runs concurrently), and emits
//! [`Event::ToolSucceeded`] or [`Event::ToolFailed`] when the task completes.
//!
//! ## Concurrency
//!
//! Each `CallTool` effect spawns an independent task — there is no sequential
//! waiting.  The kernel's single consumer loop returns immediately after
//! dispatching all effects, and tool results arrive back as events in whatever
//! order the tasks finish.  This restores the parallel-tools behaviour that was
//! lost when the old agent loop awaited each tool sequentially.
//!
//! ## `call_id → thread` registry
//!
//! An optional [`Arc<Mutex<HashMap<ToolCallId, String>>>`] maps call IDs to
//! conversation thread IDs.  When set and a mapping exists for the completing
//! call, the tool result is also appended to that thread in the shared
//! [`ThreadStore`] (append-only; never mutates prior messages).  This
//! registry is populated by the `TurnExecutor` in Phase B.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use sven_hsm::{Effect, Event, ObservationSink, ToolCallId, ToolCapability, UiEvent};
use sven_kernel::{EffectExecutor, EventSink};
use sven_vocab::provenance::ProvenanceSink;

/// Default for [`ToolExecutor::tool_timeout`].
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(600);
use crate::thread_store::ThreadStore;
use sven_model::Message;
use sven_tool_registry::{ToolCall, ToolOutput, ToolRegistry};

/// Executes [`Effect::CallTool`] using the injected [`ToolRegistry`].
pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    /// Globally-allowed capabilities (second-line defence check).
    allowed_capabilities: HashSet<ToolCapability>,
    /// Maps `call_id → (thread_id, original_call_id)`; populated by `TurnExecutor`.
    pub call_id_to_thread: Arc<Mutex<HashMap<ToolCallId, (String, String)>>>,
    /// Shared conversation store; tool results are appended here when a
    /// thread mapping exists.
    pub store: Arc<Mutex<ThreadStore>>,
    /// When set (`--no-tools`), every `CallTool` effect fails immediately
    /// instead of reaching the registry. Defence-in-depth alongside
    /// `TurnExecutor` sending an empty tool schema list: a model can't be
    /// told about tools it was never offered, but this guarantees no tool
    /// runs even if one is invoked anyway (a malformed/adversarial response).
    no_tools: bool,
    /// Maximum tokens for a single tool result before it is deterministically
    /// truncated on the way into the conversation store (see
    /// `sven_turn::smart_truncate`; category comes from
    /// `ToolRegistry::output_category`). `0` disables truncation. Mirrors
    /// `AgentConfig::tool_result_token_cap`; only affects what's stored for
    /// the model's next turn - `UiEvent::ToolFinished`/`Event::ToolSucceeded`
    /// still carry the full, untruncated output for human/audit visibility.
    tool_result_token_cap: usize,
    /// Wall-clock backstop for a single tool invocation.
    ///
    /// Nothing else bounds one: the machine advances only when the spawned
    /// task emits `ToolSucceeded`/`ToolFailed`, no machine arms a
    /// `SetTimeout` for an in-flight call, and `shell`/`run_terminal_command`
    /// take their own `timeout_secs` from model-supplied arguments with no
    /// ceiling. This is deliberately far longer than any legitimate tool so it
    /// only ever catches a hang.
    tool_timeout: Duration,
    /// Where a resolving tool's attached provenance is recorded, keyed by
    /// that tool call's own id - the handle the model can later cite as
    /// `assimilate_fact`'s `evidence` argument. `None` when no provenance
    /// index is wired in (e.g. the `memory` feature is off, or this executor
    /// serves a sub-agent kernel with no such tools registered).
    provenance_sink: Option<Arc<dyn ProvenanceSink>>,
}

impl ToolExecutor {
    /// Creates an executor that always executes tools from `registry`.
    ///
    /// Pass an empty `allowed_capabilities` to skip the redundant check
    /// (relying solely on the kernel's primary gate).
    pub fn new(registry: Arc<ToolRegistry>, allowed_capabilities: HashSet<ToolCapability>) -> Self {
        Self {
            registry,
            allowed_capabilities,
            call_id_to_thread: Arc::new(Mutex::new(HashMap::new())),
            store: Arc::new(Mutex::new(ThreadStore::new())),
            no_tools: false,
            tool_result_token_cap: 0,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            provenance_sink: None,
        }
    }

    /// Reject every `CallTool` effect instead of executing it (`--no-tools`).
    #[must_use]
    pub fn with_no_tools(mut self, no_tools: bool) -> Self {
        self.no_tools = no_tools;
        self
    }

    /// Wires a sink that a resolving tool's attached provenance is recorded
    /// into, keyed by that tool call's own id. Without this, `web_fetch`/
    /// `web_search`/`ask_question`'s attached [`sven_vocab::ToolOutput::
    /// provenance`] is simply never recorded anywhere - a later
    /// `assimilate_fact` call citing that call's id as `evidence` resolves to
    /// nothing, which is the fail-closed default (`AgentInferred`).
    #[must_use]
    pub fn with_provenance_sink(mut self, sink: Option<Arc<dyn ProvenanceSink>>) -> Self {
        self.provenance_sink = sink;
        self
    }

    /// Deterministically truncate tool results over `cap` tokens before they
    /// enter the conversation store (`0` disables truncation). See
    /// `sven_turn::smart_truncate`.
    #[must_use]
    pub fn with_tool_result_token_cap(mut self, cap: usize) -> Self {
        self.tool_result_token_cap = cap;
        self
    }

    /// Overrides the per-invocation watchdog. Mainly for tests, which cannot
    /// wait out the production default.
    #[must_use]
    pub fn with_tool_timeout(mut self, timeout: Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    /// Creates an executor with no additional capability restrictions.
    pub fn unrestricted(registry: Arc<ToolRegistry>) -> Self {
        Self::new(registry, HashSet::new())
    }

    /// Creates an executor with a pre-shared call_id registry and store.
    ///
    /// Used when `TurnExecutor` (Phase B) needs to share its registry with this
    /// executor so that tool results land on the right conversation thread.
    pub fn with_shared_store(
        registry: Arc<ToolRegistry>,
        allowed_capabilities: HashSet<ToolCapability>,
        call_id_to_thread: Arc<Mutex<HashMap<ToolCallId, (String, String)>>>,
        store: Arc<Mutex<ThreadStore>>,
    ) -> Self {
        Self {
            registry,
            allowed_capabilities,
            call_id_to_thread,
            store,
            no_tools: false,
            tool_result_token_cap: 0,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            provenance_sink: None,
        }
    }
}

#[async_trait]
impl EffectExecutor for ToolExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
        let Effect::CallTool {
            call_id,
            name,
            capability,
            args,
        } = effect
        else {
            return;
        };

        if self.no_tools {
            let _ = sink
                .emit(Event::ToolFailed {
                    call_id,
                    error: "tools are disabled for this session (--no-tools)".to_string(),
                })
                .await;
            return;
        }

        // Second-line capability check (defence-in-depth).
        if !self.allowed_capabilities.is_empty() && !self.allowed_capabilities.contains(&capability)
        {
            tracing::warn!(
                tool = %name,
                ?capability,
                "ToolExecutor: capability not in allow-list (kernel gate should have caught this)"
            );
            let _ = sink
                .emit(Event::ToolFailed {
                    call_id,
                    error: format!(
                        "capability {capability:?} is not permitted in the current context"
                    ),
                })
                .await;
            return;
        }

        let registry = Arc::clone(&self.registry);
        let call_id_to_thread = Arc::clone(&self.call_id_to_thread);
        let store = Arc::clone(&self.store);
        let sink = sink.clone();
        let obs = obs.clone();
        let tool_result_token_cap = self.tool_result_token_cap;
        let tool_timeout = self.tool_timeout;
        let provenance_sink = self.provenance_sink.clone();

        // Spawn-and-forget: the task runs concurrently with other effects.
        tokio::spawn(async move {
            // Resolve the (thread, original_call_id) mapping before running the
            // tool.  The original_call_id is the raw string the LLM returned (e.g.
            // "call_HyZJn1bTtqVbmzzS3W4VM4xb") — it must match the id recorded in
            // the preceding assistant message or the API will reject the continuation.
            let mapping = call_id_to_thread
                .lock()
                .ok()
                .and_then(|m| m.get(&call_id).cloned());

            let tool_call = ToolCall {
                id: mapping
                    .as_ref()
                    .map(|(_, orig)| orig.clone())
                    .unwrap_or_else(|| call_id.as_uuid().to_string()),
                name: name.clone(),
                args,
            };

            // Display id used for outward observation correlation: the exact
            // id the LLM assigned (matching the earlier `UiEvent::ToolStarted`),
            // falling back to the internal uuid when no mapping exists.
            let display_id = tool_call.id.clone();

            tracing::debug!(tool = %name, "ToolExecutor: invoking tool (spawned)");

            // A tool that panics or never returns must still advance the
            // machine. This task is spawn-and-forget and its `JoinHandle` is
            // dropped, so without this the `call_id` stays in `loop_core`'s
            // pending set forever and the session wedges holding its whole
            // object graph — with no error surfaced anywhere.
            let output = {
                // Shadowed inside this block only; the outer `registry` is
                // still used below for `output_category`.
                let registry = Arc::clone(&registry);
                let call = tool_call.clone();
                let task = tokio::spawn(async move { registry.execute(&call).await });
                let abort = task.abort_handle();
                match tokio::time::timeout(tool_timeout, task).await {
                    Ok(Ok(output)) => output,
                    Ok(Err(join_err)) => {
                        tracing::error!(
                            tool = %name,
                            error = %join_err,
                            "tool task did not return a result"
                        );
                        ToolOutput::err(
                            tool_call.id.clone(),
                            format!("tool '{name}' failed: {join_err}"),
                        )
                    }
                    Err(_elapsed) => {
                        abort.abort();
                        tracing::error!(
                            tool = %name,
                            timeout_secs = tool_timeout.as_secs(),
                            "tool exceeded the executor watchdog; aborted"
                        );
                        ToolOutput::err(
                            tool_call.id.clone(),
                            format!(
                                "tool '{name}' did not finish within {}s and was aborted",
                                tool_timeout.as_secs()
                            ),
                        )
                    }
                }
            };

            // A parked output has not concluded - it must not be treated as a
            // result at all. No UiEvent, no provenance, no thread-append: the
            // call is still pending, exactly as if it had never returned.
            // `Event::QuestionAsked` is where `loop_core` removes it from the
            // pending set and parks the loop; only `Event::HumanAnswered`,
            // arriving out of band, can move it forward from here.
            if let Some(parked) = output.parked {
                // Outward signal first: `UiEvent::Question` (a variant that
                // has otherwise never been constructed - see its match arms
                // across the TUI/CI/ACP frontends, all currently no-ops)
                // already reaches every consumer, so reusing it is what lets
                // a headless run recognise "parked" and stop waiting instead
                // of running until its timeout, with zero new frontend wiring.
                obs.emit(UiEvent::Question {
                    id: display_id,
                    questions: vec![parked.prompt.clone()],
                });
                let _ = sink
                    .emit(Event::QuestionAsked {
                        call_id,
                        prompt: parked.prompt,
                        options: parked.options,
                    })
                    .await;
                return;
            }

            // Emit the outward completion observation so headless / UI observers
            // can render the tool result. This mirrors `UiEvent::ToolStarted`
            // (emitted by the turn stream) and carries no inward semantics — the
            // authoritative result still flows via `Event::ToolSucceeded` /
            // `Event::ToolFailed` below.
            obs.emit(UiEvent::ToolCallFinished {
                call_id: display_id,
                tool_name: name.clone(),
                output: output.content.clone(),
                is_error: output.is_error,
            });

            // A resolving tool (`web_fetch`/`web_search`/`ask_question`)
            // attaches its own provenance to a successful result; record it
            // here, keyed by this call's own id, so a later `assimilate_fact`
            // call citing that id as `evidence` resolves to a real
            // `FactSource` instead of `AgentInferred`. Never done for a
            // failed call - there is nothing to stand behind.
            if !output.is_error {
                if let (Some(sink), Some(source)) = (&provenance_sink, &output.provenance) {
                    sink.record_provenance(&tool_call.id, (**source).clone());
                }
            }

            // Append to conversation thread if a mapping exists. Truncated
            // deterministically and content-aware (`sven_turn::smart_truncate`,
            // category from the tool's own declaration) so the stored history
            // never carries an oversized single result into every future turn
            // - unlike UiEvent::ToolFinished/Event::ToolSucceeded above, which
            // always carry the full output for human/audit visibility.
            if let Some((tid, orig)) = mapping {
                if let Ok(mut s) = store.lock() {
                    let category = registry.output_category(&name);
                    let truncated =
                        sven_turn::smart_truncate(&output.content, category, tool_result_token_cap);
                    let msg = if output.is_error {
                        Message::tool_result(orig, format!("error: {truncated}"))
                    } else {
                        Message::tool_result(orig, &truncated)
                    };
                    s.append(&tid, msg);
                }
            }

            if output.is_error {
                let _ = sink
                    .emit(Event::ToolFailed {
                        call_id,
                        error: output.content,
                    })
                    .await;
            } else {
                let observation = serde_json::Value::String(output.content);
                let _ = sink
                    .emit(Event::ToolSucceeded {
                        call_id,
                        observation,
                    })
                    .await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use sven_hsm::{
        Context, Effect, Event, Hsm, MachineId, ObservationSink, PermissionPolicy, Reaction,
        ToolCallId, ToolCapability,
    };
    use sven_kernel::{EffectExecutor, EventSink, Runtime};
    use sven_tool_registry::ToolRegistry;
    use sven_vocab::provenance::ProvenanceSink;

    use super::ToolExecutor;

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum TS {
        Top,
        Idle,
        Done,
    }

    struct OneShotMachine(MachineId);
    impl OneShotMachine {
        fn new() -> Self {
            Self(MachineId::new())
        }
    }

    impl sven_hsm::Machine for OneShotMachine {
        type State = TS;
        fn id(&self) -> MachineId {
            self.0
        }
        fn top(&self) -> TS {
            TS::Top
        }
        fn initial(&self) -> TS {
            TS::Idle
        }
        fn superstate(&self, s: TS) -> TS {
            match s {
                TS::Top => TS::Top,
                _ => TS::Top,
            }
        }
        fn is_terminal(&self, s: TS) -> bool {
            s == TS::Done
        }
        fn dispatch_state(&mut self, s: TS, e: &Event, ctx: &mut Context) -> Reaction<TS> {
            match s {
                TS::Top => Reaction::Handled(vec![]),
                TS::Idle => {
                    if e.is_lifecycle() {
                        Reaction::Handled(vec![])
                    } else {
                        ctx.set_fact("received_event_kind", format!("{:?}", e.kind()));
                        Reaction::Transition {
                            target: TS::Done,
                            effects: vec![],
                            rationale: "got domain event".into(),
                        }
                    }
                }
                TS::Done => Reaction::Handled(vec![]),
            }
        }
    }

    struct NoOpExec;
    #[async_trait::async_trait]
    impl EffectExecutor for NoOpExec {
        async fn execute(&mut self, _effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {}
    }

    pub(super) async fn run_tool_effect(exec: &mut ToolExecutor, effect: Effect) -> String {
        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        exec.execute(effect, &sink, &sven_hsm::ObservationSink::default())
            .await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        report
            .ctx
            .fact("received_event_kind")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "no event received".into())
    }

    #[tokio::test]
    async fn unknown_tool_emits_tool_failed() {
        let registry = Arc::new(ToolRegistry::new());
        let mut exec = ToolExecutor::unrestricted(registry);

        let effect = Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "nonexistent_tool_xyz".into(),
            capability: ToolCapability::ReadFile,
            args: serde_json::Value::Null,
        };
        let kind = run_tool_effect(&mut exec, effect).await;
        assert_eq!(kind, "ToolFailed");
    }

    #[tokio::test]
    async fn capability_not_in_allow_list_emits_tool_failed() {
        let registry = Arc::new(ToolRegistry::new());
        // Only allow ReadFile; trying to use ExecuteShell should fail.
        let allowed: HashSet<ToolCapability> = [ToolCapability::ReadFile].into_iter().collect();
        let mut exec = ToolExecutor::new(registry, allowed);

        let effect = Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "shell".into(),
            capability: ToolCapability::ExecuteShell,
            args: serde_json::Value::Null,
        };
        let kind = run_tool_effect(&mut exec, effect).await;
        assert_eq!(kind, "ToolFailed");
    }

    /// A registered tool that records whether it actually ran, so tests can
    /// distinguish "refused before reaching the tool" from "tool ran and
    /// happened to fail/be denied for some other reason".
    struct MarkerTool(Arc<std::sync::atomic::AtomicBool>);

    #[async_trait::async_trait]
    impl sven_tool_api::Tool for MarkerTool {
        fn name(&self) -> &str {
            "marker"
        }
        fn description(&self) -> &str {
            "test-only marker tool"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn default_policy(&self) -> sven_tool_api::ApprovalPolicy {
            sven_tool_api::ApprovalPolicy::Auto
        }
        async fn execute(&self, call: &sven_tool_api::ToolCall) -> sven_tool_api::ToolOutput {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            sven_tool_api::ToolOutput::ok(call.id.clone(), "ran")
        }
    }

    #[tokio::test]
    async fn no_tools_refuses_a_call_without_running_the_tool() {
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut registry = ToolRegistry::new();
        registry.register(MarkerTool(Arc::clone(&ran)));
        let mut exec = ToolExecutor::unrestricted(Arc::new(registry)).with_no_tools(true);

        let effect = Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "marker".into(),
            capability: ToolCapability::ReadFile,
            args: serde_json::Value::Null,
        };
        let kind = run_tool_effect(&mut exec, effect).await;
        assert_eq!(kind, "ToolFailed");
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "no_tools must short-circuit before the tool ever runs"
        );
    }

    #[tokio::test]
    async fn no_tools_false_lets_a_registered_tool_run() {
        // Sanity check for the test above: with no_tools left at its default
        // (false), the same registered tool actually executes, proving the
        // refusal above is specifically `no_tools`'s doing.
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut registry = ToolRegistry::new();
        registry.register(MarkerTool(Arc::clone(&ran)));
        let mut exec = ToolExecutor::unrestricted(Arc::new(registry));

        let effect = Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "marker".into(),
            capability: ToolCapability::ReadFile,
            args: serde_json::Value::Null,
        };
        let kind = run_tool_effect(&mut exec, effect).await;
        assert_eq!(kind, "ToolSucceeded");
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    // ── parked outputs ───────────────────────────────────────────────────────

    /// A registered tool that always parks instead of answering.
    struct ParkingTool;

    #[async_trait::async_trait]
    impl sven_tool_api::Tool for ParkingTool {
        fn name(&self) -> &str {
            "parking"
        }
        fn description(&self) -> &str {
            "test-only tool that always parks"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn default_policy(&self) -> sven_tool_api::ApprovalPolicy {
            sven_tool_api::ApprovalPolicy::Auto
        }
        async fn execute(&self, call: &sven_tool_api::ToolCall) -> sven_tool_api::ToolOutput {
            sven_tool_api::ToolOutput::parked(
                call.id.clone(),
                "Which framework?",
                vec!["Axum".into()],
            )
        }
    }

    #[tokio::test]
    async fn a_parked_output_emits_question_asked_not_tool_succeeded() {
        let mut registry = ToolRegistry::new();
        registry.register(ParkingTool);
        let mut exec = ToolExecutor::unrestricted(Arc::new(registry));

        let effect = Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "parking".into(),
            capability: ToolCapability::ReadFile,
            args: serde_json::Value::Null,
        };
        let kind = run_tool_effect(&mut exec, effect).await;
        assert_eq!(kind, "QuestionAsked");
    }

    #[tokio::test]
    async fn a_parked_output_is_not_appended_to_the_conversation_thread() {
        let mut registry = ToolRegistry::new();
        registry.register(ParkingTool);
        let mut exec = ToolExecutor::unrestricted(Arc::new(registry));
        let store = Arc::new(Mutex::new(crate::thread_store::ThreadStore::new()));
        let call_id = ToolCallId::new();

        let content = run_tool_effect_and_get_stored_content(
            &mut exec,
            Arc::clone(&store),
            "chat",
            call_id,
            "parking",
        )
        .await;
        assert_eq!(
            content, "",
            "a parked call has not concluded and must not appear as a tool result yet: {content:?}"
        );
    }

    // ── provenance_sink wiring ────────────────────────────────────────────────

    /// A registered tool that attaches provenance to a successful result, the
    /// same shape `web_fetch`/`web_search`/`ask_question` do.
    struct ResolvingTool;

    #[async_trait::async_trait]
    impl sven_tool_api::Tool for ResolvingTool {
        fn name(&self) -> &str {
            "resolving"
        }
        fn description(&self) -> &str {
            "test-only tool that attaches provenance"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn default_policy(&self) -> sven_tool_api::ApprovalPolicy {
            sven_tool_api::ApprovalPolicy::Auto
        }
        async fn execute(&self, call: &sven_tool_api::ToolCall) -> sven_tool_api::ToolOutput {
            sven_tool_api::ToolOutput::ok(call.id.clone(), "resolved")
                .with_provenance(sven_vocab::provenance::FactSource::UserStated)
        }
    }

    /// A [`ProvenanceSink`] that just records every call it received, so a
    /// test can assert on exactly what the executor forwarded.
    #[derive(Default)]
    struct RecordingSink {
        calls: Mutex<Vec<(String, sven_vocab::provenance::FactSource)>>,
    }

    impl ProvenanceSink for RecordingSink {
        fn record_provenance(&self, handle: &str, source: sven_vocab::provenance::FactSource) {
            self.calls
                .lock()
                .unwrap()
                .push((handle.to_string(), source));
        }
    }

    #[tokio::test]
    async fn a_resolving_tools_provenance_is_recorded_keyed_by_the_calls_own_id() {
        let mut registry = ToolRegistry::new();
        registry.register(ResolvingTool);
        let sink = Arc::new(RecordingSink::default());
        let mut exec = ToolExecutor::unrestricted(Arc::new(registry))
            .with_provenance_sink(Some(Arc::clone(&sink) as Arc<dyn ProvenanceSink>));

        let call_id = ToolCallId::new();
        let effect = Effect::CallTool {
            call_id,
            name: "resolving".into(),
            capability: ToolCapability::ReadFile,
            args: serde_json::Value::Null,
        };
        let kind = run_tool_effect(&mut exec, effect).await;
        assert_eq!(kind, "ToolSucceeded");

        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "exactly one provenance record");
        // No thread mapping was registered, so the tool call's id falls back
        // to the internal call_id's own uuid string - the same value the
        // model would see echoed back for this call.
        assert_eq!(calls[0].0, call_id.as_uuid().to_string());
        assert_eq!(calls[0].1, sven_vocab::provenance::FactSource::UserStated);
    }

    #[tokio::test]
    async fn a_tool_output_with_no_provenance_records_nothing() {
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut registry = ToolRegistry::new();
        registry.register(MarkerTool(Arc::clone(&ran)));
        let sink = Arc::new(RecordingSink::default());
        let mut exec = ToolExecutor::unrestricted(Arc::new(registry))
            .with_provenance_sink(Some(Arc::clone(&sink) as Arc<dyn ProvenanceSink>));

        let effect = Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "marker".into(),
            capability: ToolCapability::ReadFile,
            args: serde_json::Value::Null,
        };
        run_tool_effect(&mut exec, effect).await;
        assert!(sink.calls.lock().unwrap().is_empty());
    }

    // ── tool_result_token_cap / smart_truncate wiring ─────────────────────────

    /// A registered tool that always returns a large, fixed-category output
    /// so truncation behavior can be observed deterministically.
    struct BigOutputTool {
        category: sven_tool_api::OutputCategory,
        is_error: bool,
    }

    #[async_trait::async_trait]
    impl sven_tool_api::Tool for BigOutputTool {
        fn name(&self) -> &str {
            "big"
        }
        fn description(&self) -> &str {
            "test-only large-output tool"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn default_policy(&self) -> sven_tool_api::ApprovalPolicy {
            sven_tool_api::ApprovalPolicy::Auto
        }
        fn output_category(&self) -> sven_tool_api::OutputCategory {
            self.category
        }
        async fn execute(&self, call: &sven_tool_api::ToolCall) -> sven_tool_api::ToolOutput {
            // 100 numbered lines, comfortably over any small token cap.
            let content = (0..100)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            if self.is_error {
                sven_tool_api::ToolOutput::err(call.id.clone(), content)
            } else {
                sven_tool_api::ToolOutput::ok(call.id.clone(), content)
            }
        }
    }

    /// Runs `effect` against a `ToolExecutor` wired with a real shared store
    /// and a pre-registered call_id -> thread mapping, then returns exactly
    /// what was appended to the conversation thread - unlike
    /// `run_tool_effect`, which only reports the event kind, this is what's
    /// needed to assert on the *content* truncation produces.
    async fn run_tool_effect_and_get_stored_content(
        exec: &mut ToolExecutor,
        store: Arc<Mutex<crate::thread_store::ThreadStore>>,
        thread_id: &str,
        call_id: ToolCallId,
        tool_name: &str,
    ) -> String {
        let orig_id = call_id.as_uuid().to_string();
        exec.call_id_to_thread
            .lock()
            .unwrap()
            .insert(call_id, (thread_id.to_string(), orig_id));
        exec.store = Arc::clone(&store);

        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        exec.execute(
            Effect::CallTool {
                call_id,
                name: tool_name.into(),
                capability: ToolCapability::ReadFile,
                args: serde_json::Value::Null,
            },
            &sink,
            &sven_hsm::ObservationSink::default(),
        )
        .await;
        rt.wait_done().await;
        let _ = rt.join().await;

        // Message::as_text() only handles Text/ContentParts, not ToolResult -
        // extract directly from the tool-result content instead.
        let last = store.lock().unwrap().snapshot(thread_id).into_iter().last();
        match last.map(|m| m.content) {
            Some(sven_model::MessageContent::ToolResult { content, .. }) => {
                content.as_text().unwrap_or_default().to_string()
            }
            _ => String::new(),
        }
    }

    #[tokio::test]
    async fn tool_result_token_cap_zero_disables_truncation() {
        let mut registry = ToolRegistry::new();
        registry.register(BigOutputTool {
            category: sven_tool_api::OutputCategory::Generic,
            is_error: false,
        });
        let store = Arc::new(Mutex::new(crate::thread_store::ThreadStore::new()));
        let mut exec = ToolExecutor::unrestricted(Arc::new(registry)); // cap defaults to 0

        let content = run_tool_effect_and_get_stored_content(
            &mut exec,
            store,
            "t1",
            ToolCallId::new(),
            "big",
        )
        .await;
        assert!(
            content.contains("line 99"),
            "the full output must be stored untruncated"
        );
        assert!(
            !content.contains("omitted"),
            "no truncation notice when the cap is 0: {content}"
        );
    }

    #[tokio::test]
    async fn tool_result_token_cap_truncates_large_output() {
        let mut registry = ToolRegistry::new();
        registry.register(BigOutputTool {
            category: sven_tool_api::OutputCategory::Generic,
            is_error: false,
        });
        let store = Arc::new(Mutex::new(crate::thread_store::ThreadStore::new()));
        let mut exec =
            ToolExecutor::unrestricted(Arc::new(registry)).with_tool_result_token_cap(10);

        let content = run_tool_effect_and_get_stored_content(
            &mut exec,
            store,
            "t1",
            ToolCallId::new(),
            "big",
        )
        .await;
        assert!(
            content.contains("omitted"),
            "a truncation notice must be present: {content}"
        );
        assert!(
            !content.contains("line 99"),
            "the tail of a 100-line Generic-category output must be cut, not kept: {content}"
        );
    }

    #[tokio::test]
    async fn tool_result_token_cap_respects_output_category() {
        // HeadTail keeps both ends; Generic hard-cuts from the start only.
        // Same content, same cap, different category -> different result,
        // proving the category actually reaches smart_truncate.
        let store = Arc::new(Mutex::new(crate::thread_store::ThreadStore::new()));

        let mut generic_registry = ToolRegistry::new();
        generic_registry.register(BigOutputTool {
            category: sven_tool_api::OutputCategory::Generic,
            is_error: false,
        });
        let mut generic_exec =
            ToolExecutor::unrestricted(Arc::new(generic_registry)).with_tool_result_token_cap(30);
        let generic_content = run_tool_effect_and_get_stored_content(
            &mut generic_exec,
            Arc::clone(&store),
            "t-generic",
            ToolCallId::new(),
            "big",
        )
        .await;

        let mut headtail_registry = ToolRegistry::new();
        headtail_registry.register(BigOutputTool {
            category: sven_tool_api::OutputCategory::HeadTail,
            is_error: false,
        });
        let mut headtail_exec =
            ToolExecutor::unrestricted(Arc::new(headtail_registry)).with_tool_result_token_cap(30);
        let headtail_content = run_tool_effect_and_get_stored_content(
            &mut headtail_exec,
            store,
            "t-headtail",
            ToolCallId::new(),
            "big",
        )
        .await;

        assert!(
            !generic_content.contains("line 99"),
            "Generic must not keep the tail: {generic_content}"
        );
        assert!(
            headtail_content.contains("line 99"),
            "HeadTail must keep the tail: {headtail_content}"
        );
    }

    #[tokio::test]
    async fn tool_result_token_cap_applies_to_error_output_too() {
        let mut registry = ToolRegistry::new();
        registry.register(BigOutputTool {
            category: sven_tool_api::OutputCategory::Generic,
            is_error: true,
        });
        let store = Arc::new(Mutex::new(crate::thread_store::ThreadStore::new()));
        let mut exec =
            ToolExecutor::unrestricted(Arc::new(registry)).with_tool_result_token_cap(10);

        let content = run_tool_effect_and_get_stored_content(
            &mut exec,
            store,
            "t1",
            ToolCallId::new(),
            "big",
        )
        .await;
        assert!(
            content.starts_with("error:"),
            "error prefix must be preserved: {content}"
        );
        assert!(
            content.contains("omitted"),
            "error output must also be truncated: {content}"
        );
    }
}

#[cfg(test)]
mod watchdog_tests {
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;

    use sven_hsm::{Effect, ToolCallId};
    use sven_tool_registry::{ApprovalPolicy, Tool, ToolRegistry};

    use super::tests::run_tool_effect;
    use super::ToolExecutor;

    struct HangingTool;
    #[async_trait::async_trait]
    impl Tool for HangingTool {
        fn name(&self) -> &str {
            "hangs"
        }
        fn description(&self) -> &str {
            "never returns"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        async fn execute(&self, _call: &sven_tool_api::ToolCall) -> sven_tool_api::ToolOutput {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    struct PanickingTool;
    #[async_trait::async_trait]
    impl Tool for PanickingTool {
        fn name(&self) -> &str {
            "panics"
        }
        fn description(&self) -> &str {
            "panics"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        async fn execute(&self, _call: &sven_tool_api::ToolCall) -> sven_tool_api::ToolOutput {
            panic!("boom");
        }
    }

    fn effect(name: &str) -> Effect {
        Effect::CallTool {
            call_id: ToolCallId::new(),
            name: name.into(),
            capability: sven_hsm::ToolCapability::ReadFile,
            args: serde_json::json!({}),
        }
    }

    /// The machine only advances when the spawned task emits a result, and the
    /// task's `JoinHandle` is dropped. A tool that never returns therefore used
    /// to leave the `call_id` pending forever with nothing reported.
    #[tokio::test]
    async fn a_hanging_tool_reports_tool_failed() {
        let mut registry = ToolRegistry::new();
        registry.register(HangingTool);
        let mut exec = ToolExecutor::new(Arc::new(registry), HashSet::new())
            .with_tool_timeout(Duration::from_millis(50));

        assert_eq!(
            run_tool_effect(&mut exec, effect("hangs")).await,
            "ToolFailed",
            "a hung tool must fail the call, not stall the machine"
        );
    }

    /// Same for a panic: it happens inside the spawned task, so nothing
    /// observed it.
    #[tokio::test]
    async fn a_panicking_tool_reports_tool_failed() {
        let mut registry = ToolRegistry::new();
        registry.register(PanickingTool);
        let mut exec = ToolExecutor::new(Arc::new(registry), HashSet::new());

        assert_eq!(
            run_tool_effect(&mut exec, effect("panics")).await,
            "ToolFailed",
            "a panicking tool must fail the call, not stall the machine"
        );
    }
}
