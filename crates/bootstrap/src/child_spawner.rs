// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Production [`ChildSpawner`] for the SDLC deliberation engine.
//!
//! [`SdlcChildSpawner`] services `Effect::InstantiateSubmachine` (emitted by
//! [`SdlcMachine`](sven_machines::SdlcMachine) when an approved plan decomposes into
//! independent tasks). For each task it builds a **fully isolated** child
//! kernel: a fresh [`Context`], a fresh [`TurnExecutor`] (hence a fresh
//! append-only conversation store), running a one-shot
//! [`TaskMachine`](sven_machines::TaskMachine). The children run concurrently on
//! their own tokio tasks; when one ends, its structured result is posted back
//! to the parent as `Event::Internal(SubmachineCompleted { result })` so the
//! parent can aggregate it (append-only) on the execution thread.
//!
//! # The child's contract
//!
//! A child runs under the contract the kernel hands down - what the parent
//! holds in the state that spawned it - narrowed by this spawner's own terms
//! for a task:
//!
//! - capabilities: read, write, git, shell and network, each only if the
//!   parent holds it. Shell stays approval-gated in the child as everywhere.
//! - tool rounds: the task's own cap, lowered to the session's
//!   `agent.max_tool_rounds` when that is smaller.
//! - wall clock: `agent.child_run_timeout_secs`, never later than the
//!   parent's own deadline.
//!
//! The child is stopped when its parent is cancelled or shut down, or when
//! its deadline passes; its tool calls in flight are aborted with it.
//!
//! # Reaching a person
//!
//! When the parent session has question and approval channels (see
//! [`SdlcChildSpawner::with_gates`]), a child's questions and approval
//! requests - a decision that needs input or approval, a tool call that needs
//! approval - go to those same channels, and the child waits for the answer
//! within its deadline. Without them, a task that needs a person ends with
//! `ok: false` and says so.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use sven_config::Config;
use sven_executors::{
    ApprovalRequest, CompositeExecutorBuilder, ThreadStore, ToolExecutor, TurnExecutor,
    UserExecutor, UserQuestion,
};
use sven_hsm::{
    event::InternalEvent, submachine::ErasedMachine, ChildRunContract, Context, Event, Hsm,
    MachineId, PermissionPolicy, ToolCallId, ToolCapability,
};
use sven_kernel::{ChildRun, ChildSpawner, ErasedRuntime, EventSink, SystemClock};
use sven_machines::{TaskMachine, MAX_TOOL_ROUNDS_FACT};
use sven_tool_registry::ToolRegistry;

/// The parent session's human-gate channels, shared with its children.
#[derive(Clone)]
struct Gates {
    questions: mpsc::Sender<UserQuestion>,
    approvals: mpsc::Sender<ApprovalRequest>,
}

/// Spawns isolated child task kernels for parallel SDLC execution.
pub struct SdlcChildSpawner {
    default_model: Arc<dyn sven_model::ModelProvider>,
    config: Arc<Config>,
    tool_registry: Arc<ToolRegistry>,
    gates: Option<Gates>,
}

impl SdlcChildSpawner {
    /// Create a spawner sharing the parent's model, config, and tool registry.
    #[must_use]
    pub fn new(
        default_model: Arc<dyn sven_model::ModelProvider>,
        config: Arc<Config>,
        tool_registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            default_model,
            config,
            tool_registry,
            gates: None,
        }
    }

    /// Lets children ask the parent's person: their questions and approval
    /// requests go to these channels, the same ones the parent session's own
    /// gates use.
    #[must_use]
    pub fn with_gates(
        mut self,
        questions: mpsc::Sender<UserQuestion>,
        approvals: mpsc::Sender<ApprovalRequest>,
    ) -> Self {
        self.gates = Some(Gates {
            questions,
            approvals,
        });
        self
    }

    /// What a task child may ever do, whatever its parent holds: basic
    /// read/write/shell/git/network work, and not starting children of its
    /// own. The kernel gates every `CallTool` effect against the narrowed
    /// policy and answers a call outside it with `ToolFailed`.
    fn child_terms(&self, now: Instant) -> ChildRunContract {
        let policy = PermissionPolicy::builder()
            .allow_globally([
                ToolCapability::ReadFile,
                ToolCapability::WriteFile,
                ToolCapability::GitOperation,
                ToolCapability::ExecuteShell,
                ToolCapability::NetworkAccess,
            ])
            .build();
        let mut terms =
            ChildRunContract::new(policy).with_max_tool_rounds(self.config.agent.max_tool_rounds);
        // A child never writes more per response than the session is
        // configured to, whatever model a state switches it to.
        terms.max_output_tokens = self.config.model.max_output_tokens;
        match self.config.agent.child_run_timeout() {
            Some(budget) => terms.with_deadline_after(now, budget),
            None => terms,
        }
    }

    /// The contract a task child runs under: what it inherited, narrowed by
    /// this spawner's terms.
    fn contract_for(&self, inherited: &ChildRunContract, now: Instant) -> ChildRunContract {
        inherited.narrow(&self.child_terms(now))
    }

    /// The child's executor: its own turn loop and tool slot, the parent's
    /// gates when there are any, all stopped with the child's run.
    fn executor(
        &self,
        contract: &ChildRunContract,
        run: &ChildRun,
    ) -> sven_executors::CompositeExecutor {
        let conv_store = Arc::new(std::sync::Mutex::new(ThreadStore::new()));
        let call_id_to_thread = Arc::new(std::sync::Mutex::new(HashMap::<
            ToolCallId,
            (String, String),
        >::new()));
        // The turn executor's abort slot is per child; the run itself is
        // stopped through its cancel scope, which drops the turn in flight.
        let turn_abort_slot = Arc::new(tokio::sync::Mutex::new(None));

        let resolver_config = Arc::clone(&self.config);
        // Deliberately `from_config`, not `from_config_probed`: `ModelResolver`
        // (`sven_turn::stream_turn::ModelResolver`) is a synchronous `Fn`,
        // called synchronously from `TurnExecutor::resolve_model`, and
        // `from_config_probed` needs an `.await`. The primary session model IS
        // probed (`runtime_builder.rs::build`); only a per-state/per-child
        // model override (this path) is not.
        let model_resolver: sven_turn::ModelResolver = Arc::new(move |model_str: &str| {
            let model_cfg = sven_model::resolve_model_from_config(&resolver_config, model_str);
            let provider = sven_model_drivers::from_config(&model_cfg)?;
            Ok(Arc::from(provider) as Arc<dyn sven_model::ModelProvider>)
        });

        let turn_executor = TurnExecutor::new(
            self.default_model.clone(),
            Some(model_resolver),
            Arc::clone(&self.tool_registry),
            Arc::clone(&conv_store),
            Arc::clone(&call_id_to_thread),
            turn_abort_slot,
        )
        .with_turn_limits(
            sven_turn::TurnLimits::from_agent_config(&self.config.agent)
                .with_max_output_tokens(contract.max_output_tokens),
        );

        let tool_executor = ToolExecutor::with_shared_store(
            Arc::clone(&self.tool_registry),
            Default::default(),
            call_id_to_thread,
            conv_store,
        )
        .with_cancel(run.cancel.clone());

        let builder = CompositeExecutorBuilder::default()
            .with_turn(turn_executor)
            .with_tool_executor(tool_executor)
            .with_timers(Arc::new(SystemClock::new()));
        match &self.gates {
            Some(gates) => builder.with_user_slot(Box::new(UserExecutor::new(
                gates.questions.clone(),
                gates.approvals.clone(),
            ))),
            None => builder,
        }
        .build()
    }
}

/// What a child that was stopped before finishing reports.
fn stopped_result(task: &str, contract: &ChildRunContract) -> Value {
    let reason = match contract.remaining(Instant::now()) {
        Some(left) if left.is_zero() => "the task ran out of its wall-clock budget",
        _ => "the task was cancelled with the run that started it",
    };
    json!({"task": task, "summary": reason, "ok": false})
}

#[async_trait]
impl ChildSpawner for SdlcChildSpawner {
    async fn spawn_child(
        &self,
        machine: MachineId,
        descriptor: Value,
        run: ChildRun,
        parent: EventSink,
    ) {
        let task = descriptor
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let contract = self.contract_for(&run.contract, Instant::now());
        let run = ChildRun {
            contract: contract.clone(),
            cancel: run.cancel,
        };

        let mut ctx = Context::new();
        if let Some(rounds) = contract.max_tool_rounds {
            ctx.set_fact(MAX_TOOL_ROUNDS_FACT, rounds);
        }
        ctx.set_fact(TaskMachine::HUMAN_GATE_FACT, self.gates.is_some());

        let executor = self.executor(&contract, &run);
        let rt = ErasedRuntime::spawn_child_run(
            Box::new(Hsm::new(TaskMachine::new(task.clone()))) as Box<dyn ErasedMachine>,
            ctx,
            executor,
            64,
            None,
            run,
        );

        let machine_id = machine.as_uuid().to_string();
        // Spawn-and-forget: harvest the child's result on its own task so the
        // parent loop is never blocked and siblings run concurrently.
        tokio::spawn(async move {
            rt.wait_done().await;
            let result = match rt.join().await {
                Ok(report) => report
                    .ctx
                    .fact(TaskMachine::RESULT_FACT)
                    .cloned()
                    .unwrap_or_else(|| stopped_result(&task, &contract)),
                Err(e) => {
                    json!({"task": task, "summary": format!("task run failed: {e}"), "ok": false})
                }
            };
            let _ = parent
                .emit(Event::Internal(InternalEvent::SubmachineCompleted {
                    machine: machine_id,
                    result,
                }))
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sven_hsm::{Machine, Reaction};
    use sven_machines::machines::sdlc::SdlcState;
    use sven_machines::SdlcMachine;
    use sven_model::ResponseEvent;
    use sven_model_mock::ScriptedMockProvider;

    use super::*;

    fn spawner(model: ScriptedMockProvider, config: Config) -> SdlcChildSpawner {
        SdlcChildSpawner::new(
            Arc::new(model),
            Arc::new(config),
            Arc::new(ToolRegistry::new()),
        )
    }

    #[test]
    fn a_task_child_holds_no_more_than_the_parent_state_that_spawned_it() {
        let spawner = spawner(ScriptedMockProvider::new(vec![]), Config::default());
        let parent = SdlcMachine::permission_policy();
        let now = Instant::now();

        let planning = spawner.contract_for(
            &ChildRunContract::inherit(&parent, &SdlcState::Planning),
            now,
        );
        for cap in [
            ToolCapability::WriteFile,
            ToolCapability::ExecuteShell,
            ToolCapability::NetworkAccess,
            ToolCapability::SpawnChild,
        ] {
            assert!(!planning.policy.allows_in_every_state(cap), "{cap:?}");
        }

        let execution = spawner.contract_for(
            &ChildRunContract::inherit(&parent, &SdlcState::Execution),
            now,
        );
        assert!(execution
            .policy
            .allows_in_every_state(ToolCapability::WriteFile));
        assert!(execution
            .policy
            .allows_in_every_state(ToolCapability::ExecuteShell));
        assert!(
            !execution
                .policy
                .allows_in_every_state(ToolCapability::NetworkAccess),
            "the parent holds no network, so neither does the child"
        );
        assert!(!execution
            .policy
            .allows_in_every_state(ToolCapability::SpawnChild));
        assert_eq!(
            execution.remaining(now),
            Some(Duration::from_secs(
                Config::default().agent.child_run_timeout_secs
            ))
        );
    }

    #[test]
    fn a_task_child_writes_no_more_per_response_than_the_session() {
        let mut config = Config::default();
        config.model.max_output_tokens = Some(2048);
        let spawner = spawner(ScriptedMockProvider::new(vec![]), config);
        let parent = SdlcMachine::permission_policy();
        let child = spawner.contract_for(
            &ChildRunContract::inherit(&parent, &SdlcState::Execution),
            Instant::now(),
        );
        assert_eq!(child.max_output_tokens, Some(2048));
    }

    /// Records the one child result it is sent.
    struct Collector(MachineId);

    impl Machine for Collector {
        type State = bool;
        fn id(&self) -> MachineId {
            self.0
        }
        fn top(&self) -> bool {
            false
        }
        fn initial(&self) -> bool {
            false
        }
        fn superstate(&self, _s: bool) -> bool {
            false
        }
        fn is_terminal(&self, s: bool) -> bool {
            s
        }
        fn all_states(&self) -> Vec<bool> {
            vec![false, true]
        }
        fn dispatch_state(&mut self, _s: bool, event: &Event, ctx: &mut Context) -> Reaction<bool> {
            match event {
                Event::Internal(InternalEvent::SubmachineCompleted { result, .. }) => {
                    ctx.set_fact("child", result.clone());
                    Reaction::transition(true, [], "child reported")
                }
                _ => Reaction::Ignored,
            }
        }
    }

    fn decision(decision: Value) -> Vec<ResponseEvent> {
        vec![
            ResponseEvent::TextDelta(decision.to_string()),
            ResponseEvent::Done,
        ]
    }

    #[tokio::test]
    async fn a_child_asks_the_parent_gate_and_completes_with_the_answer() {
        let model = ScriptedMockProvider::new(vec![
            decision(json!({"status": "need_user_input", "message": "Which database?"})),
            decision(json!({"status": "proceed", "message": "used the answer"})),
        ]);
        let last_request = Arc::clone(&model.last_request);
        let (questions, mut asked) = mpsc::channel(4);
        let (approvals, _approvals_rx) = mpsc::channel(4);
        let spawner = spawner(model, Config::default()).with_gates(questions, approvals);

        let parent = ErasedRuntime::spawn(
            Box::new(Hsm::new(Collector(MachineId::new()))) as Box<dyn ErasedMachine>,
            Context::new(),
            PermissionPolicy::builder().build(),
            sven_executors::CompositeExecutorBuilder::default().build(),
            8,
        );
        let run = ChildRun {
            contract: ChildRunContract::inherit(
                &SdlcMachine::permission_policy(),
                &SdlcState::Execution,
            ),
            cancel: parent.cancel_scope().child(),
        };
        spawner
            .spawn_child(
                MachineId::new(),
                json!({"task": "store users"}),
                run,
                parent.sink(),
            )
            .await;

        let question = tokio::time::timeout(Duration::from_secs(10), asked.recv())
            .await
            .expect("the child asks")
            .expect("question channel open");
        assert!(
            question.prompt.contains("Which database?"),
            "{}",
            question.prompt
        );
        question.reply_tx.send("Postgres".into()).unwrap();

        tokio::time::timeout(Duration::from_secs(10), parent.wait_done())
            .await
            .expect("the child reports to its parent");
        let report = parent.join().await.unwrap();
        let result = report.ctx.fact("child").cloned().unwrap();
        assert_eq!(result["ok"], json!(true), "{result}");
        let sent = format!("{:?}", last_request.lock().unwrap());
        assert!(
            sent.contains("Postgres"),
            "the answer reached the child's model"
        );
    }
    #[tokio::test]
    async fn a_cancelled_child_withdraws_its_pending_question() {
        let model = ScriptedMockProvider::new(vec![decision(
            json!({"status": "need_user_input", "message": "Which database?"}),
        )]);
        let (questions, mut asked) = mpsc::channel(4);
        let (approvals, _approvals_rx) = mpsc::channel(4);
        let spawner = spawner(model, Config::default()).with_gates(questions, approvals);
        let parent = ErasedRuntime::spawn(
            Box::new(Hsm::new(Collector(MachineId::new()))) as Box<dyn ErasedMachine>,
            Context::new(),
            PermissionPolicy::builder().build(),
            sven_executors::CompositeExecutorBuilder::default().build(),
            8,
        );
        let cancel = parent.cancel_scope().child();
        let run = ChildRun {
            contract: ChildRunContract::inherit(
                &SdlcMachine::permission_policy(),
                &SdlcState::Execution,
            ),
            cancel: cancel.clone(),
        };
        spawner
            .spawn_child(MachineId::new(), json!({"task": "t"}), run, parent.sink())
            .await;
        let mut question = tokio::time::timeout(Duration::from_secs(10), asked.recv())
            .await
            .expect("the child asks")
            .expect("question channel open");
        assert!(!question.reply_tx.is_closed());

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(10), question.reply_tx.closed())
            .await
            .expect("the question is withdrawn with the child");
        tokio::time::timeout(Duration::from_secs(10), parent.wait_done())
            .await
            .expect("the child still reports to its parent");
        let result = parent
            .join()
            .await
            .unwrap()
            .ctx
            .fact("child")
            .cloned()
            .unwrap();
        assert_eq!(result["ok"], json!(false), "{result}");
    }
}
