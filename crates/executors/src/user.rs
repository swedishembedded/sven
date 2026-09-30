//! User interaction effect executors.
//!
//! Handles [`Effect::AskUser`], [`Effect::RequestHumanApproval`] and
//! [`Effect::RequestHumanAnswer`] by forwarding them to a caller-supplied
//! channel.  The other end of the channel is held by the TUI, the CI
//! headless runner, or any other frontend.
//!
//! # Protocol
//!
//! * [`Effect::AskUser`] → sends a [`UserQuestion`] (with a `reply_tx`
//!   oneshot) to the `question_tx` channel. A background task awaits the
//!   reply and posts `Event::UserMessage { text }` to the kernel queue.
//! * [`Effect::RequestHumanApproval`] → sends an [`ApprovalRequest`] (with
//!   a `reply_tx` oneshot) to the `approval_tx` channel. A background task
//!   awaits the reply (`true` = approved) and posts `Event::HumanApproved`
//!   or `Event::HumanRejected`.
//! * [`Effect::RequestHumanAnswer`] → sends a [`ParkedQuestion`] to the
//!   `parked_tx` channel and returns immediately - **no reply is awaited
//!   here**. Unlike the two effects above, the run may sit parked for a long
//!   time (the point of parking rather than blocking), so nothing in this
//!   process holds a `oneshot` open waiting for it. Resolution happens out of
//!   band, in a wholly separate flow: whatever durably records the question
//!   (a queue, a ledger) later resolves it and posts `Event::HumanAnswered`
//!   directly to the kernel sink, from code that may not even be running in
//!   this process. Configuring no `parked_tx` at all is a valid, honest
//!   configuration (a deployment with no async-escalation sink yet) - the
//!   question is logged and dropped, exactly like any other unconfigured
//!   [`CompositeExecutor`](crate::composite::CompositeExecutor) slot; the run
//!   simply stays parked forever rather than being answered on its behalf.
//!
//! # Knowledge assimilation
//!
//! This executor is the only place in the process that sees a human answer an
//! approval request, so it is where an approval for
//! [`ToolCapability::AssimilateKnowledge`] is recorded into the shared
//! [`KnowledgeApprovals`] handle the `assimilate_fact` tool consults. Deriving
//! it from the real `HumanApproved` event here - rather than from a flag in a
//! tool call - is what makes the gate a gate.

use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{
    ApprovalId, Effect, Event, ObservationSink, QuestionId, ToolCallId, ToolCapability,
};
use sven_kernel::{EffectExecutor, EventSink};
use sven_vocab::provenance::KnowledgeApprovals;
use tokio::sync::{mpsc, oneshot};

// ── Channel message types ─────────────────────────────────────────────────────

/// A question forwarded to the frontend.
pub struct UserQuestion {
    /// Text shown to the user.
    pub prompt: String,
    /// Send the user's reply back here.
    pub reply_tx: oneshot::Sender<String>,
}

/// A question parked awaiting a human answer, forwarded to whatever durably
/// records it. Deliberately carries no `reply_tx` - see the module doc for
/// why this is not a request/reply exchange.
pub struct ParkedQuestion {
    /// Identifies this question; a later `Event::HumanAnswered` must match it.
    pub question_id: QuestionId,
    /// The tool call this question was asked on behalf of.
    pub call_id: ToolCallId,
    /// The question text shown to the human.
    pub prompt: String,
    /// Offered choices, if any (empty for a free-form question).
    pub options: Vec<String>,
}

/// An approval request forwarded to the frontend.
pub struct ApprovalRequest {
    /// Identifies this approval for matching.
    pub approval_id: ApprovalId,
    /// What capability is being requested.
    pub capability: ToolCapability,
    /// Human-readable description of what will happen.
    pub description: String,
    /// The tool call the approval gates, when it gates one.
    pub call: Option<sven_hsm::GatedCall>,
    /// Send `true` (approved) or `false` (rejected) back here.
    pub reply_tx: oneshot::Sender<bool>,
}

// ── UserExecutor ──────────────────────────────────────────────────────────────

/// Executes [`Effect::AskUser`] and [`Effect::RequestHumanApproval`].
pub struct UserExecutor {
    question_tx: mpsc::Sender<UserQuestion>,
    approval_tx: mpsc::Sender<ApprovalRequest>,
    parked_tx: Option<mpsc::Sender<ParkedQuestion>>,
    knowledge_approvals: Option<Arc<KnowledgeApprovals>>,
}

impl UserExecutor {
    /// Creates an executor.
    ///
    /// `question_tx` receives [`UserQuestion`] messages; the holder must read
    /// each one, display the prompt to the user, and send the reply through
    /// `UserQuestion::reply_tx`.
    ///
    /// `approval_tx` receives [`ApprovalRequest`] messages; the holder must
    /// present the request to the user and send `true`/`false` through
    /// `ApprovalRequest::reply_tx`.
    pub fn new(
        question_tx: mpsc::Sender<UserQuestion>,
        approval_tx: mpsc::Sender<ApprovalRequest>,
    ) -> Self {
        Self {
            question_tx,
            approval_tx,
            parked_tx: None,
            knowledge_approvals: None,
        }
    }

    /// Records approvals for [`ToolCapability::AssimilateKnowledge`] into
    /// `approvals`, which the `assimilate_fact` tool reads before letting
    /// web-sourced content become durable knowledge.
    #[must_use]
    pub fn with_knowledge_approvals(mut self, approvals: Arc<KnowledgeApprovals>) -> Self {
        self.knowledge_approvals = Some(approvals);
        self
    }

    /// Routes [`Effect::RequestHumanAnswer`] to `tx` instead of dropping it.
    /// Without this, a parked question is logged and discarded - the run
    /// still parks (nothing resumes it), but no durable record exists to
    /// resume it from later.
    #[must_use]
    pub fn with_parked_questions(mut self, tx: mpsc::Sender<ParkedQuestion>) -> Self {
        self.parked_tx = Some(tx);
        self
    }

    /// Creates both ends of the question channel. Returns the executor side
    /// and the frontend side (`mpsc::Receiver<UserQuestion>`).
    pub fn question_channel(
        cap: usize,
    ) -> (mpsc::Sender<UserQuestion>, mpsc::Receiver<UserQuestion>) {
        mpsc::channel(cap)
    }

    /// Creates both ends of the approval channel. Returns the executor side
    /// and the frontend side (`mpsc::Receiver<ApprovalRequest>`).
    pub fn approval_channel(
        cap: usize,
    ) -> (
        mpsc::Sender<ApprovalRequest>,
        mpsc::Receiver<ApprovalRequest>,
    ) {
        mpsc::channel(cap)
    }

    /// Creates both ends of the parked-question channel. Returns the executor
    /// side and the durable-recording side (`mpsc::Receiver<ParkedQuestion>`).
    pub fn parked_channel(
        cap: usize,
    ) -> (mpsc::Sender<ParkedQuestion>, mpsc::Receiver<ParkedQuestion>) {
        mpsc::channel(cap)
    }
}

#[async_trait]
impl EffectExecutor for UserExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        match effect {
            Effect::AskUser { prompt } => {
                let (reply_tx, reply_rx) = oneshot::channel();
                let question = UserQuestion { prompt, reply_tx };
                if self.question_tx.send(question).await.is_err() {
                    tracing::warn!("UserExecutor: question channel closed; emitting empty reply");
                    let _ = sink
                        .emit(Event::UserMessage {
                            text: String::new(),
                        })
                        .await;
                    return;
                }
                let sink = sink.clone();
                tokio::spawn(async move {
                    match reply_rx.await {
                        Ok(text) => {
                            let _ = sink.emit(Event::UserMessage { text }).await;
                        }
                        Err(_) => {
                            tracing::warn!("UserExecutor: reply oneshot dropped before answer");
                        }
                    }
                });
            }

            Effect::RequestHumanApproval {
                approval_id,
                capability,
                description,
                call,
            } => {
                let (reply_tx, reply_rx) = oneshot::channel();
                let req = ApprovalRequest {
                    approval_id,
                    capability,
                    description,
                    call,
                    reply_tx,
                };
                if self.approval_tx.send(req).await.is_err() {
                    tracing::warn!("UserExecutor: approval channel closed; emitting HumanRejected");
                    let _ = sink.emit(Event::HumanRejected { approval_id }).await;
                    return;
                }
                let sink = sink.clone();
                let knowledge_approvals = self.knowledge_approvals.clone();
                tokio::spawn(async move {
                    match reply_rx.await {
                        Ok(true) => {
                            // The only place a human approval is observed, so
                            // the only honest place to record one.
                            if capability == ToolCapability::AssimilateKnowledge {
                                if let Some(approvals) = &knowledge_approvals {
                                    approvals.record_human_approval();
                                }
                            }
                            let _ = sink.emit(Event::HumanApproved { approval_id }).await;
                        }
                        Ok(false) => {
                            let _ = sink.emit(Event::HumanRejected { approval_id }).await;
                        }
                        Err(_) => {
                            tracing::warn!(
                                "UserExecutor: approval oneshot dropped; emitting HumanRejected"
                            );
                            let _ = sink.emit(Event::HumanRejected { approval_id }).await;
                        }
                    }
                });
            }

            Effect::RequestHumanAnswer {
                question_id,
                call_id,
                prompt,
                options,
            } => {
                if let Some(tx) = &self.parked_tx {
                    let parked = ParkedQuestion {
                        question_id,
                        call_id,
                        prompt,
                        options,
                    };
                    if tx.send(parked).await.is_err() {
                        tracing::warn!(
                            ?question_id,
                            "UserExecutor: parked-question channel closed; question dropped, run stays parked"
                        );
                    }
                } else {
                    tracing::warn!(
                        ?question_id,
                        "UserExecutor: no parked-question sink configured; question dropped, run stays parked"
                    );
                }
                // Deliberately no event posted and no task spawned: unlike
                // AskUser/RequestHumanApproval, nothing here waits for the
                // reply. See the module doc.
            }

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use sven_hsm::{
        ApprovalId, Context, Effect, Event, Hsm, MachineId, ObservationSink, PermissionPolicy,
        Reaction, ToolCapability,
    };
    use sven_kernel::{EffectExecutor, EventSink, Runtime};
    use tokio::sync::mpsc;

    use super::{ApprovalRequest, UserExecutor, UserQuestion};

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
                        return Reaction::Handled(vec![]);
                    }
                    ctx.set_fact("received_event_kind", format!("{:?}", e.kind()));
                    Reaction::Transition {
                        target: TS::Done,
                        effects: vec![],
                        rationale: "got event".into(),
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

    async fn run_user_effect(exec: &mut UserExecutor, effect: Effect) -> String {
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

    /// Runs `effect` and asserts the machine is **never** driven to a
    /// terminal state - i.e. no kernel event ever arrives for it. Unlike
    /// [`run_user_effect`] (which awaits `wait_done()` unconditionally and
    /// would hang forever here), this races the wait against a short timeout,
    /// since a real absence-of-event is indistinguishable from "still
    /// pending" without one.
    async fn assert_no_kernel_event(exec: &mut UserExecutor, effect: Effect) {
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
        let outcome =
            tokio::time::timeout(std::time::Duration::from_millis(200), rt.wait_done()).await;
        rt.abort();
        assert!(
            outcome.is_err(),
            "no kernel event should ever be posted for this effect, but the machine reached a terminal state"
        );
    }

    #[tokio::test]
    async fn ask_user_emits_user_message_with_reply() {
        let (q_tx, mut q_rx) = mpsc::channel::<UserQuestion>(4);
        let (a_tx, _a_rx) = mpsc::channel::<ApprovalRequest>(4);
        let mut exec = UserExecutor::new(q_tx, a_tx);

        let effect = Effect::AskUser {
            prompt: "What is your name?".into(),
        };

        // Spawn a task that plays the role of the TUI: receives the question
        // and sends a reply.
        tokio::spawn(async move {
            let q = q_rx.recv().await.unwrap();
            q.reply_tx.send("Alice".into()).unwrap();
        });

        let kind = run_user_effect(&mut exec, effect).await;
        assert_eq!(kind, "UserMessage");
    }

    #[tokio::test]
    async fn request_approval_emits_human_approved_when_accepted() {
        let (q_tx, _q_rx) = mpsc::channel::<UserQuestion>(4);
        let (a_tx, mut a_rx) = mpsc::channel::<ApprovalRequest>(4);
        let mut exec = UserExecutor::new(q_tx, a_tx);

        let approval_id = ApprovalId::new();
        let effect = Effect::RequestHumanApproval {
            approval_id,
            capability: ToolCapability::ExecuteShell,
            description: "Run build script".into(),
            call: None,
        };

        tokio::spawn(async move {
            let req = a_rx.recv().await.unwrap();
            req.reply_tx.send(true).unwrap();
        });

        let kind = run_user_effect(&mut exec, effect).await;
        assert_eq!(kind, "HumanApproved");
    }

    #[tokio::test]
    async fn request_approval_emits_human_rejected_when_denied() {
        let (q_tx, _q_rx) = mpsc::channel::<UserQuestion>(4);
        let (a_tx, mut a_rx) = mpsc::channel::<ApprovalRequest>(4);
        let mut exec = UserExecutor::new(q_tx, a_tx);

        let approval_id = ApprovalId::new();
        let effect = Effect::RequestHumanApproval {
            approval_id,
            capability: ToolCapability::DeleteFile,
            description: "Delete temp files".into(),
            call: None,
        };

        tokio::spawn(async move {
            let req = a_rx.recv().await.unwrap();
            req.reply_tx.send(false).unwrap();
        });

        let kind = run_user_effect(&mut exec, effect).await;
        assert_eq!(kind, "HumanRejected");
    }

    #[tokio::test]
    async fn request_human_answer_forwards_to_the_parked_sink_and_posts_no_event() {
        use sven_hsm::QuestionId;

        let (q_tx, _q_rx) = mpsc::channel::<UserQuestion>(4);
        let (a_tx, _a_rx) = mpsc::channel::<ApprovalRequest>(4);
        let (p_tx, mut p_rx) = UserExecutor::parked_channel(4);
        let mut exec = UserExecutor::new(q_tx, a_tx).with_parked_questions(p_tx);

        let question_id = QuestionId::new();
        let call_id = sven_hsm::ToolCallId::new();
        let effect = Effect::RequestHumanAnswer {
            question_id,
            call_id,
            prompt: "Which framework?".into(),
            options: vec!["Axum".into(), "Actix".into()],
        };

        // No kernel event should ever arrive for this effect - the answer is
        // wholly out of band.
        assert_no_kernel_event(&mut exec, effect).await;

        let parked = p_rx.recv().await.expect("parked question forwarded");
        assert_eq!(parked.question_id, question_id);
        assert_eq!(parked.call_id, call_id);
        assert_eq!(parked.prompt, "Which framework?");
        assert_eq!(
            parked.options,
            vec!["Axum".to_string(), "Actix".to_string()]
        );
    }

    #[tokio::test]
    async fn request_human_answer_with_no_sink_configured_drops_without_panicking() {
        use sven_hsm::QuestionId;

        let (q_tx, _q_rx) = mpsc::channel::<UserQuestion>(4);
        let (a_tx, _a_rx) = mpsc::channel::<ApprovalRequest>(4);
        let mut exec = UserExecutor::new(q_tx, a_tx); // no with_parked_questions

        let effect = Effect::RequestHumanAnswer {
            question_id: QuestionId::new(),
            call_id: sven_hsm::ToolCallId::new(),
            prompt: "x".into(),
            options: vec![],
        };
        // Must not panic (no sink configured) and must still post no event.
        assert_no_kernel_event(&mut exec, effect).await;
    }

    #[tokio::test]
    async fn ask_user_with_closed_channel_emits_empty_user_message() {
        let (q_tx, q_rx) = mpsc::channel::<UserQuestion>(4);
        let (a_tx, _a_rx) = mpsc::channel::<ApprovalRequest>(4);
        drop(q_rx); // close the receiver
        let mut exec = UserExecutor::new(q_tx, a_tx);

        let effect = Effect::AskUser {
            prompt: "Hello?".into(),
        };
        let kind = run_user_effect(&mut exec, effect).await;
        assert_eq!(kind, "UserMessage");
    }
}
