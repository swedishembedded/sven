// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The one controller of a session's lifecycle.
//!
//! Every surface that runs a conversation - the interactive TUI, the headless
//! runner, the ACP server - needs the same few things done the same way: a
//! kernel session assembled from the configuration, its events streamed out,
//! its questions and approvals answered by whoever the surface has for that,
//! the conversation carried over when the mode or the model changes, and the
//! session closed. [`SessionController`] does them, and is the only code
//! outside `sven-bootstrap` that assembles a [`RuntimeBuilder`]: a surface
//! describes what it is ([`SessionOptions`]: where its questions go, whether
//! a person approves calls, what the project is) and what it wants to run
//! ([`SessionSpec`]: the machine, the permissions, the model), and renders
//! the [`AgentEvent`]s that come back.
//!
//! The verbs are the SDK's lifecycle ones - open, send, change, suspend,
//! close - over a session that stays alive between turns, which an
//! interactive surface needs and the SDK's per-turn `Agent` deliberately does
//! not keep.
//!
//! Swedish Embedded AB implements session control for agent frontends for
//! its clients. If your team needs expertise in running one agent session
//! behind a terminal, an editor and a pipeline alike then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::fmt;
use std::sync::Arc;

use anyhow::Context as _;
use sven_bootstrap::session_handles::{HumanGateResponder, RuntimeHandle};
use sven_bootstrap::{Config, KernelAgentSession, McpManager, RuntimeBuilder, RuntimeContext};
use sven_machines::AgentEvent;
use sven_mcp_client::McpEvent;
use sven_model::Message;
use sven_model_drivers::ModelConfig;
use sven_tool_api::PermissionRequester;
use sven_tools_agent::QuestionRequest;
use sven_vocab::{AgentMode, ApprovalMode};
use tokio::sync::{mpsc, oneshot, Mutex};

/// The slot a turn's abort sender is parked in; taking it interrupts the
/// model call in flight.
pub type AbortSlot = Arc<Mutex<Option<oneshot::Sender<()>>>>;

/// Who answers a session's questions and approvals.
pub enum Gates {
    /// Nobody is there: every question is answered at once with
    /// [`NO_USER_ANSWER`](sven_tool_api::NO_USER_ANSWER), and no approval is
    /// given on anyone's behalf. A headless run.
    Unattended,
    /// A person at a modal: each question and approval is a
    /// [`QuestionRequest`] on this channel, and the model's `ask_question`
    /// tool asks through it too. The TUI.
    Modal(mpsc::Sender<QuestionRequest>),
    /// A host that answers each gate itself, with the gated call and its
    /// capability in hand. An ACP server answering its client.
    Host(HumanGateResponder),
}

/// What a surface is, and so what stays the same through every rebuild of
/// its session.
pub struct SessionOptions {
    /// The configuration the session is built from.
    pub config: Arc<Config>,
    /// The project the session works in, and what was found about it.
    pub runtime: RuntimeContext,
    /// Whether a person approves each call that is not read-only.
    pub approval: ApprovalMode,
    /// Who answers questions and approvals.
    pub gates: Gates,
    /// Where the session's events go. Every build of the session - the
    /// first and each rebuild - sends here, so the receiver outlives them.
    pub events: mpsc::Sender<AgentEvent>,
    /// Whether an MCP server that needs it may open a browser to sign in.
    /// Off for a run nobody is watching.
    pub allow_interactive_oauth: bool,
    /// How long to wait, in milliseconds, for MCP tools before the session
    /// starts. `None` does not wait.
    pub wait_for_mcp_tools_ms: Option<u64>,
    /// Asks the surface's host before a tool whose policy is `Ask` runs.
    pub permission_requester: Option<Arc<dyn PermissionRequester>>,
    /// Where the turn executor parks the sender that interrupts the model
    /// call in flight.
    pub abort_slot: Option<AbortSlot>,
    /// Test seam: the provider to run on instead of the one the model
    /// configuration names. See `ProviderFactory`.
    pub(crate) provider_factory: Option<ProviderFactory>,
}

/// Test seam: maps a session's model and permissions to a concrete provider.
///
/// Production leaves it unset and the session builds its provider from the
/// model configuration. Tests inject distinguishable mock providers to assert
/// that a model or mode change actually re-drives the kernel through the
/// intended one.
pub(crate) type ProviderFactory = Box<
    dyn Fn(&ModelConfig, AgentMode) -> Option<Box<dyn sven_model::ModelProvider>> + Send + Sync,
>;

impl SessionOptions {
    /// A surface with nobody to ask, under auto approval: the options every
    /// other surface starts from.
    #[must_use]
    pub fn new(
        config: Arc<Config>,
        runtime: RuntimeContext,
        events: mpsc::Sender<AgentEvent>,
    ) -> Self {
        Self {
            config,
            runtime,
            approval: ApprovalMode::Auto,
            gates: Gates::Unattended,
            events,
            allow_interactive_oauth: true,
            wait_for_mcp_tools_ms: None,
            permission_requester: None,
            abort_slot: None,
            provider_factory: None,
        }
    }
}

/// What a session runs: which machine, under which permissions, on which
/// model. Changing any of them is a rebuild of the session.
#[derive(Clone, Debug)]
pub struct SessionSpec {
    /// The kernel machine, by its name in the mode registry.
    pub machine: String,
    /// The permissions the session runs under: `Plan` and `Research` forbid
    /// writing.
    pub permissions: AgentMode,
    /// The model.
    pub model: ModelConfig,
}

impl SessionSpec {
    /// The session `mode` names, on `model`.
    #[must_use]
    pub fn for_mode(mode: AgentMode, model: ModelConfig) -> Self {
        Self {
            machine: machine_for(mode).to_string(),
            permissions: mode,
            model,
        }
    }

    /// This session with `mode` and `model` where given.
    #[must_use]
    fn with_changes(&self, mode: Option<AgentMode>, model: Option<ModelConfig>) -> Self {
        let mut spec = match mode {
            Some(mode) => Self::for_mode(mode, self.model.clone()),
            None => self.clone(),
        };
        if let Some(model) = model {
            spec.model = model;
        }
        spec
    }

    /// Whether running `other` takes a different kernel: another machine,
    /// other permissions, or another provider, model or endpoint. A model
    /// that differs only in its limits does not.
    fn needs_rebuild_for(&self, other: &Self) -> bool {
        self.machine != other.machine
            || self.permissions != other.permissions
            || self.model.provider != other.model.provider
            || self.model.name != other.model.name
            || self.model.base_url != other.model.base_url
    }
}

/// The kernel machine that runs `mode`: the reactive `agent` machine drives
/// every coding-oriented mode, and what tells `plan` and `research` apart from
/// `agent` is their permissions.
#[must_use]
pub fn machine_for(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Chat => "chat",
        AgentMode::Sdlc => "sdlc",
        AgentMode::Agent | AgentMode::Plan | AgentMode::Research => "agent",
    }
}

/// Why a message could not be sent.
#[derive(Debug)]
pub enum SubmitError {
    /// The session could not be rebuilt for the mode or model asked for.
    Rebuild(anyhow::Error),
    /// The kernel's event queue is closed: the session is over.
    QueueClosed,
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rebuild(e) => write!(f, "{e:#}"),
            Self::QueueClosed => f.write_str("kernel queue closed"),
        }
    }
}

impl std::error::Error for SubmitError {}

/// A live session and everything needed to rebuild it.
///
/// Dropping it shuts the session down.
pub struct SessionController {
    options: SessionOptions,
    spec: SessionSpec,
    session: KernelAgentSession,
    /// The one MCP manager, kept across rebuilds so its connections - and
    /// the servers the agent added - survive a change of mode or model.
    mcp: Arc<McpManager>,
}

impl SessionController {
    /// Opens a session running `spec`, seeded with `history`.
    ///
    /// Returns the MCP event receiver beside it: the session's MCP manager
    /// reports server events there, for a surface that shows them.
    ///
    /// # Errors
    ///
    /// The kernel session cannot be built.
    pub async fn open(
        options: SessionOptions,
        spec: SessionSpec,
        history: Vec<Message>,
    ) -> anyhow::Result<(Self, mpsc::Receiver<McpEvent>)> {
        let (session, mcp_events) = assemble(&options, &spec, history, None).await?;
        let mcp = session.mcp_manager();
        Ok((
            Self {
                options,
                spec,
                session,
                mcp,
            },
            mcp_events,
        ))
    }

    /// What the session runs now.
    #[must_use]
    pub fn spec(&self) -> &SessionSpec {
        &self.spec
    }

    /// What the session was opened with.
    #[must_use]
    pub fn options(&self) -> &SessionOptions {
        &self.options
    }

    /// Replaces the session with one running `spec`, carrying the
    /// conversation so far and the MCP servers. Never while a turn runs: the
    /// turn would be left waiting on the old session.
    ///
    /// # Errors
    ///
    /// The kernel session cannot be built; the old one is kept.
    pub async fn rebuild(&mut self, spec: SessionSpec) -> anyhow::Result<()> {
        let history = self.session.history_snapshot();
        self.rebuild_with(spec, history).await
    }

    async fn rebuild_with(
        &mut self,
        spec: SessionSpec,
        history: Vec<Message>,
    ) -> anyhow::Result<()> {
        let (session, _mcp_events) =
            assemble(&self.options, &spec, history, Some(Arc::clone(&self.mcp))).await?;
        self.session = session;
        self.spec = spec;
        Ok(())
    }

    /// Posts `text` as the user's next message, under `mode` and `model`
    /// where given. A mode or model that takes a different kernel rebuilds
    /// the session first, with the conversation so far.
    ///
    /// # Errors
    ///
    /// [`SubmitError::Rebuild`] when the rebuild fails, and
    /// [`SubmitError::QueueClosed`] when the session is over.
    pub async fn submit(
        &mut self,
        text: String,
        mode: Option<AgentMode>,
        model: Option<ModelConfig>,
    ) -> Result<(), SubmitError> {
        let target = self.spec.with_changes(mode, model);
        if self.spec.needs_rebuild_for(&target) {
            self.rebuild(target).await.map_err(SubmitError::Rebuild)?;
        } else {
            self.spec = target;
        }
        self.post(text).await
    }

    /// [`Self::submit`] over a conversation the surface has edited: the next
    /// turn sees exactly `messages`, whether they seed the live session or the
    /// rebuilt one.
    ///
    /// # Errors
    ///
    /// As [`Self::submit`].
    pub async fn resubmit(
        &mut self,
        messages: Vec<Message>,
        text: String,
        mode: Option<AgentMode>,
        model: Option<ModelConfig>,
    ) -> Result<(), SubmitError> {
        let target = self.spec.with_changes(mode, model);
        if self.spec.needs_rebuild_for(&target) {
            self.rebuild_with(target, messages)
                .await
                .map_err(SubmitError::Rebuild)?;
        } else {
            self.spec = target;
            self.session.seed_history(messages);
        }
        self.post(text).await
    }

    /// Posts `text` as the user's next message to the session as it is.
    ///
    /// # Errors
    ///
    /// [`SubmitError::QueueClosed`] when the session is over.
    pub async fn post(&self, text: String) -> Result<(), SubmitError> {
        if self.session.send_user_message(text).await {
            Ok(())
        } else {
            Err(SubmitError::QueueClosed)
        }
    }

    /// Interrupts the turn in flight. `false` when the session is over.
    pub async fn cancel(&self) -> bool {
        self.session.cancel().await
    }

    /// Replaces the conversation with `messages`, for the next turn.
    pub fn seed_history(&self, messages: Vec<Message>) {
        self.session.seed_history(messages);
    }

    /// The conversation so far.
    #[must_use]
    pub fn history(&self) -> Vec<Message> {
        self.session.history_snapshot()
    }

    /// The session's MCP manager.
    #[must_use]
    pub fn mcp_manager(&self) -> Arc<McpManager> {
        Arc::clone(&self.mcp)
    }

    /// The session's live tool registry.
    #[must_use]
    pub fn tool_registry(&self) -> Arc<sven_tool_registry::ToolRegistry> {
        self.session.tool_registry()
    }

    /// Reloads the registry's MCP tools from the manager, so tools that
    /// appeared after the session started are usable without a rebuild.
    pub async fn refresh_mcp_tools(&self) {
        self.session.refresh_mcp_tools().await;
    }

    /// A handle on the kernel: its event sink, status and observations, for
    /// a surface that watches more than the event stream.
    #[must_use]
    pub fn handle(&self) -> RuntimeHandle {
        self.session.handle()
    }

    /// Lets the kernel finish what it has queued after the controller is
    /// gone, which is what flushes the audit log at the end of a one-shot
    /// run.
    pub fn detach(self) {
        self.session.detach();
    }

    /// Waits for a machine that ends - verified-task - and returns what it
    /// recorded.
    ///
    /// # Errors
    ///
    /// The kernel task panicked or was cancelled.
    pub async fn join(self) -> Result<sven_hsm::ErasedReport, tokio::task::JoinError> {
        self.session.join().await
    }
}

/// The one place a session is assembled: a [`RuntimeBuilder`] set up from
/// `options` and `spec`, started on `history` and, when given, on an
/// existing MCP manager, and wired to whoever answers its gates.
async fn assemble(
    options: &SessionOptions,
    spec: &SessionSpec,
    history: Vec<Message>,
    mcp: Option<Arc<McpManager>>,
) -> anyhow::Result<(KernelAgentSession, mpsc::Receiver<McpEvent>)> {
    let mut builder = RuntimeBuilder::new(Arc::clone(&options.config), spec.machine.clone())
        .with_runtime_context(options.runtime.clone())
        .with_model_config(spec.model.clone())
        .with_agent_mode(spec.permissions)
        .with_approval_mode(options.approval)
        .with_allow_interactive_oauth(options.allow_interactive_oauth)
        .with_initial_history(history);
    if let Some(timeout_ms) = options.wait_for_mcp_tools_ms {
        builder = builder.with_wait_for_mcp_tools(timeout_ms);
    }
    if let Gates::Modal(questions) = &options.gates {
        builder = builder.with_tool_question_tx(questions.clone());
    }
    if let Some(slot) = &options.abort_slot {
        builder = builder.with_cancel_handle(Arc::clone(slot));
    }
    if let Some(requester) = &options.permission_requester {
        builder = builder.with_permission_requester(Arc::clone(requester));
    }
    if let Some(mcp) = mcp {
        builder = builder.with_mcp_manager(mcp);
    }
    if let Some(provider) = options
        .provider_factory
        .as_ref()
        .and_then(|factory| factory(&spec.model, spec.permissions))
    {
        builder = builder.with_model_provider(provider);
    }
    let bundle = builder
        .build_session()
        .await
        .context("kernel session init")?;
    let events = options.events.clone();
    Ok(match &options.gates {
        Gates::Unattended => KernelAgentSession::spawn_unattended(bundle, events),
        Gates::Modal(questions) => KernelAgentSession::spawn(bundle, events, questions.clone()),
        Gates::Host(responder) => {
            KernelAgentSession::spawn_answering(bundle, events, Arc::clone(responder))
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use sven_model::ResponseEvent;
    use sven_model_mock::ScriptedMockProvider;

    use super::*;

    fn config() -> Arc<Config> {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();
        Arc::new(config)
    }

    fn model(name: &str) -> ModelConfig {
        ModelConfig {
            provider: "mock".into(),
            name: name.into(),
            ..ModelConfig::default()
        }
    }

    /// Options whose provider answers every turn with `reply`, and a counter
    /// of how many sessions were built on it.
    fn options(
        reply: &'static str,
    ) -> (SessionOptions, mpsc::Receiver<AgentEvent>, Arc<AtomicUsize>) {
        let (events, rx) = mpsc::channel(256);
        let builds = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&builds);
        let mut options = SessionOptions::new(config(), RuntimeContext::empty(), events);
        options.provider_factory = Some(Box::new(move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            Some(Box::new(ScriptedMockProvider::always_text(reply)))
        }));
        (options, rx, builds)
    }

    /// Everything the session emits until the turn ends.
    async fn turn(rx: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            let done = matches!(event, AgentEvent::TurnComplete);
            out.push(event);
            if done {
                break;
            }
        }
        out
    }

    #[test]
    fn the_reactive_machine_runs_every_coding_mode() {
        for mode in [AgentMode::Agent, AgentMode::Plan, AgentMode::Research] {
            assert_eq!(machine_for(mode), "agent", "{mode}");
        }
        assert_eq!(machine_for(AgentMode::Chat), "chat");
        assert_eq!(machine_for(AgentMode::Sdlc), "sdlc");
    }

    #[tokio::test]
    async fn the_same_mode_and_model_keep_the_session() {
        let (options, mut rx, builds) = options("hello");
        let spec = SessionSpec::for_mode(AgentMode::Agent, model("a"));
        let (mut session, _mcp) = SessionController::open(options, spec, vec![])
            .await
            .unwrap();

        session.submit("one".into(), None, None).await.unwrap();
        turn(&mut rx).await;
        session
            .submit("two".into(), Some(AgentMode::Agent), Some(model("a")))
            .await
            .unwrap();
        turn(&mut rx).await;

        assert_eq!(builds.load(Ordering::SeqCst), 1, "no rebuild");
    }

    #[tokio::test]
    async fn a_new_mode_rebuilds_the_session_with_the_conversation_and_the_mcp_manager() {
        let (options, mut rx, builds) = options("hello");
        let spec = SessionSpec::for_mode(AgentMode::Agent, model("a"));
        let (mut session, _mcp) = SessionController::open(options, spec, vec![])
            .await
            .unwrap();
        let manager = session.mcp_manager();

        session
            .submit("remember this".into(), None, None)
            .await
            .unwrap();
        turn(&mut rx).await;
        session
            .submit("again".into(), Some(AgentMode::Plan), None)
            .await
            .unwrap();
        turn(&mut rx).await;

        assert_eq!(builds.load(Ordering::SeqCst), 2, "one rebuild");
        assert_eq!(session.spec().permissions, AgentMode::Plan);
        assert!(
            Arc::ptr_eq(&manager, &session.mcp_manager()),
            "same MCP manager"
        );
        let history = session.history();
        assert!(
            history.iter().any(|m| m.as_text() == Some("remember this")),
            "the first turn survives the rebuild: {history:?}"
        );
    }

    #[tokio::test]
    async fn a_new_model_rebuilds_the_session_but_new_limits_alone_do_not() {
        let (options, mut rx, builds) = options("hello");
        let spec = SessionSpec::for_mode(AgentMode::Agent, model("a"));
        let (mut session, _mcp) = SessionController::open(options, spec, vec![])
            .await
            .unwrap();

        let tighter = ModelConfig {
            max_tokens: Some(1000),
            ..model("a")
        };
        session
            .submit("one".into(), None, Some(tighter.clone()))
            .await
            .unwrap();
        turn(&mut rx).await;
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(
            session.spec().model.max_tokens,
            Some(1000),
            "the limits are kept"
        );

        session
            .submit("two".into(), None, Some(model("b")))
            .await
            .unwrap();
        turn(&mut rx).await;
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert_eq!(session.spec().model.name, "b");
    }

    #[tokio::test]
    async fn a_resubmitted_conversation_replaces_the_one_the_session_held() {
        let (options, mut rx, _builds) = options("ok");
        let spec = SessionSpec::for_mode(AgentMode::Agent, model("a"));
        let (mut session, _mcp) = SessionController::open(options, spec, vec![])
            .await
            .unwrap();
        session.submit("original".into(), None, None).await.unwrap();
        turn(&mut rx).await;

        session
            .resubmit(
                vec![Message::user("edited"), Message::assistant("answer")],
                "follow up".into(),
                None,
                None,
            )
            .await
            .unwrap();
        turn(&mut rx).await;

        let history = session.history();
        assert!(history.iter().any(|m| m.as_text() == Some("edited")));
        assert!(!history.iter().any(|m| m.as_text() == Some("original")));
    }

    /// The model asks one question; the host's gates decide who answers it.
    fn asks_then_concludes() -> Vec<Vec<ResponseEvent>> {
        let ask = serde_json::json!({"questions": [{"prompt": "Which framework?", "options": ["Axum", "Actix"]}]});
        vec![
            vec![
                ResponseEvent::ToolCall {
                    index: 0,
                    id: "q1".into(),
                    name: "ask_question".into(),
                    arguments: ask.to_string(),
                },
                ResponseEvent::Done,
            ],
            vec![
                ResponseEvent::TextDelta("carrying on".into()),
                ResponseEvent::Done,
            ],
        ]
    }

    fn question_result(events: &[AgentEvent]) -> String {
        events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallFinished {
                    tool_name, output, ..
                } if tool_name == "ask_question" => Some(output.clone()),
                _ => None,
            })
            .expect("the question was answered")
    }

    /// A turn in which the model asks to write `path`.
    fn writes(path: &std::path::Path) -> ProviderFactory {
        let args =
            serde_json::json!({"path": path.to_string_lossy(), "text": "written", "append": false})
                .to_string();
        Box::new(move |_, _| {
            Some(Box::new(ScriptedMockProvider::tool_then_text(
                "call-w",
                "write_file",
                args.clone(),
                "done",
            )))
        })
    }

    #[tokio::test]
    async fn under_manual_approval_nobody_approves_a_call_on_a_persons_behalf() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("probe.txt");
        let (mut options, mut rx, _builds) = options("unused");
        options.approval = ApprovalMode::Manual;
        options.gates = Gates::Unattended;
        options.provider_factory = Some(writes(&target));
        let spec = SessionSpec::for_mode(AgentMode::Agent, model("a"));
        let (session, _mcp) = SessionController::open(options, spec, vec![])
            .await
            .unwrap();

        session.post("write it".into()).await.unwrap();
        turn(&mut rx).await;

        assert!(!target.exists(), "an unattended session approved a write");
    }

    #[tokio::test]
    async fn under_manual_approval_a_call_runs_once_the_person_at_the_modal_approves_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("probe.txt");
        let (mut options, mut rx, _builds) = options("unused");
        let (question_tx, mut questions) = mpsc::channel::<QuestionRequest>(4);
        options.approval = ApprovalMode::Manual;
        options.gates = Gates::Modal(question_tx);
        options.provider_factory = Some(writes(&target));
        let spec = SessionSpec::for_mode(AgentMode::Agent, model("a"));
        let (session, _mcp) = SessionController::open(options, spec, vec![])
            .await
            .unwrap();

        session.post("write it".into()).await.unwrap();
        let asked = tokio::time::timeout(Duration::from_secs(30), questions.recv())
            .await
            .expect("the approval arrives")
            .expect("the channel is open");
        assert!(asked.questions[0].prompt.contains("write_file"));
        asked.answer_tx.send("yes".into()).unwrap();
        turn(&mut rx).await;

        assert!(target.exists(), "the approved write ran");
    }

    #[tokio::test]
    async fn a_question_at_a_modal_reaches_the_person_and_their_answer_the_model() {
        let (mut options, mut rx, _builds) = options("unused");
        let (question_tx, mut questions) = mpsc::channel::<QuestionRequest>(4);
        options.gates = Gates::Modal(question_tx);
        options.provider_factory = Some(Box::new(|_, _| {
            Some(Box::new(ScriptedMockProvider::new(asks_then_concludes())))
        }));
        let spec = SessionSpec::for_mode(AgentMode::Agent, model("a"));
        let (session, _mcp) = SessionController::open(options, spec, vec![])
            .await
            .unwrap();

        session.post("which?".into()).await.unwrap();
        let asked = tokio::time::timeout(Duration::from_secs(30), questions.recv())
            .await
            .expect("the question arrives")
            .expect("the channel is open");
        assert_eq!(asked.questions[0].prompt, "Which framework?");
        asked.answer_tx.send("Axum".into()).unwrap();
        let events = turn(&mut rx).await;

        assert!(question_result(&events).contains("Axum"), "{events:?}");
    }
}
