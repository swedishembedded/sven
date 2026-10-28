// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven rc`: this machine's agent sessions, run for a remote-control server.
//!
//! Three kinds of task, connected only by channels:
//!
//! * the **command loop** reads the server's command stream (reconnecting
//!   with backoff from the last command it took) and routes each command to
//!   its session, starting the session's actor on first sight;
//! * one **session actor** per session owns that session's controller,
//!   handles its commands one at a time and forwards its events;
//! * the **uploader** batches every session's events, numbers them, and
//!   uploads them in order, retrying a batch until the server stores it.
//!
//! A session actor that fails reports the failure as a session event and
//! keeps serving; nothing one session does can stop another.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use sven_bootstrap::{Config, RuntimeContext};
use sven_control::{ControlCommand, ControlEvent, HistoryEntry, SessionState};
use sven_frontend::{CompactError, SessionController, SessionOptions, SessionSpec};
use sven_machines::AgentEvent;
use sven_model::Message;
use sven_model_drivers::ModelConfig;
use sven_vocab::AgentMode;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::approvals::{PendingApprovals, RemoteApprovals, APPROVAL_TIMEOUT};
use crate::client::{EnvironmentTaken, RemoteClient, TokenRefused};
use crate::login::RemoteLogin;
use crate::sessions::{opening, SessionStore, CONTINUE_INTERRUPTED};
use upload::{upload_loop, Upload};

mod describe;
mod slash;
mod upload;

/// Events waiting for the uploader. Full means the server has stopped
/// taking them; sessions then wait rather than grow memory without bound.
const UPLOAD_QUEUE: usize = 4096;
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Commands queued for one session while it works.
const SESSION_QUEUE: usize = 64;
/// A kept session unused this long is forgotten when the host starts.
const KEPT_UNUSED: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// What `sven rc` runs with.
pub struct HostOptions {
    pub config: Arc<Config>,
    /// Where every session works.
    pub working_dir: PathBuf,
    pub login: RemoteLogin,
    /// Where sessions are kept between turns and across restarts.
    pub sessions: SessionStore,
    /// A server's hosted host: attaches as the `hosted` environment, not as
    /// a directory of a person's machine.
    pub hosted: bool,
}

/// The only tools a hosted host's sessions have (`sven rc --hosted`): none
/// of them reads, writes or runs anything on the machine the host runs on.
pub const HOSTED_TOOLS: &[&str] = &["web_search", "web_fetch", "skill"];

/// The system prompt of a hosted conversation when the site configures none
/// (`agent.system_prompt` in the file `sven --config` reads).
pub const HOSTED_SYSTEM_PROMPT: &str = "You are sven, a helpful assistant. Answer clearly and \
     concisely, and say so when you are not sure. You can search the web and read public web \
     pages; say which sources you used. You have no access to the person's computer or files.";

/// What a session works with. On a person's computer: the project it runs in,
/// whose root, git state and context file the built-in prompt describes, with
/// a configured `agent.system_prompt` as its identity. Hosted: the configured
/// prompt (or [`HOSTED_SYSTEM_PROMPT`]) and nothing else, since the working
/// directory is the site's own sandbox, not something to tell a visitor about.
fn session_runtime(config: &Config, working_dir: &Path, hosted: bool) -> RuntimeContext {
    let mut runtime = RuntimeContext::auto_detect_at(Some(working_dir.to_path_buf()));
    runtime.project_root = Some(working_dir.to_path_buf());
    if hosted {
        runtime.no_system = true;
        runtime.system_prompt_override = Some(
            config
                .agent
                .system_prompt
                .clone()
                .unwrap_or_else(|| HOSTED_SYSTEM_PROMPT.to_string()),
        );
    } else {
        runtime.system_prompt_override = config.agent.system_prompt.clone();
    }
    runtime
}

/// What a hosted host's sessions may do, applied to its configuration: only
/// [`HOSTED_TOOLS`] (an allow list, so a tool added later is never offered
/// unless named there), and `web_fetch` reaching only the public internet,
/// never the server's own network (its services, its database, a cloud
/// metadata service).
pub fn restrict_to_hosted(config: &mut Config) {
    config.tools.only = Some(
        HOSTED_TOOLS
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
    );
    config.tools.web.public_only = true;
}

/// Runs until interrupted, or until the server stops accepting the token.
///
/// # Errors
///
/// The token is refused, at start or later.
pub async fn run_host(options: HostOptions) -> anyhow::Result<()> {
    // One host serves one directory: its sessions' commands start there and
    // their relative paths resolve there, wherever this process started.
    std::env::set_current_dir(&options.working_dir)
        .map_err(|e| anyhow::anyhow!("cannot work in {}: {e}", options.working_dir.display()))?;
    let mut client = RemoteClient::new(&options.login.control_url, &options.login.token)?;
    let who = client.whoami().await?;
    // This run, which the environment is held for and the uploads are
    // numbered in.
    let run = Uuid::new_v4();
    let path = if options.hosted {
        sven_control::remote::HOSTED_ENVIRONMENT.to_string()
    } else {
        options.working_dir.display().to_string()
    };
    client.attach_environment(&path, run).await?;
    // What it kept for conversations nobody has used for a month goes.
    let pruned = options.sessions.prune(KEPT_UNUSED);
    if pruned > 0 {
        tracing::info!("forgot {pruned} session(s) unused for 30 days");
    }
    println!(
        "Connected to {} as \"{}\" for {}. Sessions work in {}. Ctrl-C stops.",
        options.login.server,
        who.workspace_name,
        who.account,
        options.working_dir.display()
    );

    let (uploads, upload_rx) = mpsc::channel(UPLOAD_QUEUE);
    tokio::spawn(upload_loop(client.clone(), upload_rx, run));
    // Permission requests travel with the sessions' events, in order.
    let (asks, mut asks_rx) = mpsc::channel::<ControlEvent>(SESSION_QUEUE);
    let ask_uploads = uploads.clone();
    tokio::spawn(async move {
        while let Some(ask) = asks_rx.recv().await {
            if ask_uploads
                .send(Upload::Event(Box::new(ask)))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let context = Arc::new(SessionContext {
        config: options.config,
        working_dir: options.working_dir,
        hosted: options.hosted,
        openai_base_url: options.login.openai_base_url,
        token: options.login.token,
        uploads: uploads.clone(),
        sessions: options.sessions,
        asks,
    });

    // What a session here can run, so the server can offer it as it is typed.
    list_slash_commands(&context).await;

    let (routed, routed_rx) = mpsc::channel(SESSION_QUEUE);
    tokio::spawn(router(routed_rx, Arc::clone(&context)));
    println!("Waiting for conversations. To write to one, run `sven rc attach <id>`.");

    tokio::select! {
        result = command_loop(&client, &routed, &uploads) => result,
        _ = tokio::signal::ctrl_c() => {
            println!("Stopping. Sessions in progress end here.");
            if let Err(e) = client.detach_environment().await {
                tracing::warn!("could not tell the server this host stopped: {e:#}");
            }
            Ok(())
        }
    }
}

async fn command_loop(
    client: &RemoteClient,
    routed: &mpsc::Sender<ControlCommand>,
    uploads: &mpsc::Sender<Upload>,
) -> anyhow::Result<()> {
    let mut taken = 0u64;
    let mut backoff = Duration::from_millis(500);
    loop {
        match client.commands(taken).await {
            Ok(stream) => {
                backoff = Duration::from_millis(500);
                futures::pin_mut!(stream);
                while let Some(item) = stream.next().await {
                    let envelope = match item {
                        Ok(envelope) => envelope,
                        Err(e) => {
                            tracing::warn!("command stream: {e:#}");
                            break;
                        }
                    };
                    // At-least-once delivery: a command already taken is
                    // sent again after a reconnect and dropped here.
                    if envelope.seq <= taken {
                        continue;
                    }
                    taken = envelope.seq;
                    let _ = routed.send(envelope.command).await;
                    let _ = uploads.send(Upload::Ack(taken)).await;
                }
            }
            // Signed out, or replaced by another `sven rc` here: retrying
            // cannot help, and two hosts must never run one environment.
            Err(e)
                if e.downcast_ref::<TokenRefused>().is_some()
                    || e.downcast_ref::<EnvironmentTaken>().is_some() =>
            {
                return Err(e)
            }
            Err(e) => tracing::warn!("command stream: {e:#}"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Owns the sessions: starts each one's actor on first sight and hands it its
/// commands. The host takes no input of its own: a person writes to a
/// conversation from wherever it is open (`sven rc attach`, the server).
async fn router(mut rx: mpsc::Receiver<ControlCommand>, context: Arc<SessionContext>) {
    let mut sessions: HashMap<Uuid, mpsc::Sender<SessionCommand>> = HashMap::new();
    while let Some(command) = rx.recv().await {
        route(command, &mut sessions, &context).await;
    }
}

/// Hands `command` to its session, starting the session's actor when this is
/// the first the host hears of it.
async fn route(
    command: ControlCommand,
    sessions: &mut HashMap<Uuid, mpsc::Sender<SessionCommand>>,
    context: &Arc<SessionContext>,
) {
    let (session_id, message) = match command {
        ControlCommand::NewSession {
            id,
            mode,
            history,
            replace_history,
            ..
        } => (
            id,
            SessionCommand::Open {
                mode,
                history,
                replace: replace_history,
            },
        ),
        ControlCommand::SetModel { session_id, model } => {
            (session_id, SessionCommand::Model(model))
        }
        ControlCommand::SendInput { session_id, text } => (session_id, SessionCommand::Input(text)),
        ControlCommand::CancelSession { session_id } => (session_id, SessionCommand::Cancel),
        ControlCommand::ApproveTool {
            session_id,
            call_id,
        } => (
            session_id,
            SessionCommand::Decide {
                call_id,
                approved: true,
            },
        ),
        ControlCommand::DenyTool {
            session_id,
            call_id,
            ..
        } => (
            session_id,
            SessionCommand::Decide {
                call_id,
                approved: false,
            },
        ),
        ControlCommand::ListSlashCommands => {
            list_slash_commands(context).await;
            return;
        }
        ControlCommand::CloseSession { session_id } => {
            // A session this run started ends itself; one it never started
            // has only what an earlier run kept.
            match sessions.remove(&session_id) {
                Some(actor) => {
                    let _ = actor.send(SessionCommand::Close).await;
                }
                None => {
                    if let Err(e) = context.sessions.forget(session_id) {
                        tracing::warn!("session {session_id}: could not forget it: {e:#}");
                    }
                }
            }
            return;
        }
        other => {
            tracing::debug!("rc ignores {other:?}");
            return;
        }
    };
    let actor = sessions.entry(session_id).or_insert_with(|| {
        let (tx, rx) = mpsc::channel(SESSION_QUEUE);
        tokio::spawn(SessionActor::new(session_id, Arc::clone(context)).run(rx));
        tx
    });
    if actor.send(message).await.is_err() {
        tracing::warn!("session {session_id} has stopped; its command is dropped");
    }
}

/// Tells the server which slash commands a session here can run. The list
/// is read again each time: a command file added since shows up.
async fn list_slash_commands(context: &SessionContext) {
    let commands = slash::available(&context.working_dir);
    let event = ControlEvent::SlashCommands { commands };
    if context
        .uploads
        .send(Upload::Event(Box::new(event)))
        .await
        .is_err()
    {
        tracing::warn!("the uploader has stopped; the slash command list is dropped");
    }
}

/// What every session shares.
struct SessionContext {
    config: Arc<Config>,
    working_dir: PathBuf,
    /// Run by a site for its own web conversations (`sven rc --hosted`).
    hosted: bool,
    openai_base_url: String,
    token: String,
    uploads: mpsc::Sender<Upload>,
    /// Where each session's own conversation is kept between turns.
    sessions: SessionStore,
    /// Where a session's tool calls that need permission are asked about.
    asks: mpsc::Sender<ControlEvent>,
}

enum SessionCommand {
    Open {
        mode: AgentMode,
        history: Vec<HistoryEntry>,
        /// The server took turns back: `history` replaces the conversation
        /// of a session already running, instead of being ignored.
        replace: bool,
    },
    Model(String),
    Input(String),
    /// The server's answer to a tool call waiting for permission.
    Decide {
        call_id: String,
        approved: bool,
    },
    Cancel,
    /// Its conversation was archived or deleted: end it and forget it.
    Close,
}

/// One session: its controller, created on the first input, and what it
/// needs to create one.
struct SessionActor {
    id: Uuid,
    context: Arc<SessionContext>,
    mode: AgentMode,
    model: Option<String>,
    history: Vec<Message>,
    /// The request whose answer the host was stopped in the middle of, kept
    /// from before a restart; asked again, it continues that answer.
    unfinished: Option<String>,
    controller: Option<SessionController>,
    /// A turn or a compaction is running: the last state told to the server
    /// was `Running`.
    working: bool,
    /// Closed: nothing is kept for it any more, even by an answer that was
    /// still finishing.
    closed: bool,
    /// This session's tool calls waiting for the server's permission.
    approvals: PendingApprovals,
    /// Each running tool call's description and when it started, to tell
    /// the server how long it ran.
    running: HashMap<String, (Instant, describe::Described)>,
    events_tx: mpsc::Sender<AgentEvent>,
    events_rx: mpsc::Receiver<AgentEvent>,
}

impl SessionActor {
    fn new(id: Uuid, context: Arc<SessionContext>) -> Self {
        let (events_tx, events_rx) = mpsc::channel(1024);
        Self {
            id,
            context,
            mode: AgentMode::Agent,
            model: None,
            history: Vec::new(),
            unfinished: None,
            controller: None,
            working: false,
            closed: false,
            approvals: PendingApprovals::default(),
            running: HashMap::new(),
            events_tx,
            events_rx,
        }
    }

    fn short(&self) -> String {
        self.id.to_string()[..8].to_string()
    }

    async fn run(mut self, mut commands: mpsc::Receiver<SessionCommand>) {
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.handle(command).await,
                    None => break,
                },
                Some(event) = self.events_rx.recv() => self.forward(event).await,
            }
        }
    }

    async fn handle(&mut self, command: SessionCommand) {
        match command {
            SessionCommand::Open {
                mode,
                history,
                replace,
            } => {
                if replace {
                    self.replace_conversation(mode, sven_executors::history_messages(history))
                        .await;
                } else if self.controller.is_none() {
                    // Idempotent: a running session keeps its own conversation.
                    self.mode = mode;
                    // Its own record when this host ran it before (tool calls
                    // included), else the server's flat text.
                    let opened = opening(
                        self.context.sessions.load(self.id),
                        sven_executors::history_messages(history),
                    );
                    self.history = opened.history;
                    self.unfinished = opened.unfinished;
                }
            }
            SessionCommand::Model(model) => self.model = Some(model),
            SessionCommand::Input(text) => self.answer(text).await,
            SessionCommand::Decide { call_id, approved } => {
                if !self.approvals.decide(&call_id, approved) {
                    tracing::debug!(
                        "session {}: no call {call_id} waits for permission",
                        self.id
                    );
                }
            }
            SessionCommand::Cancel => {
                if let Some(controller) = &self.controller {
                    controller.cancel().await;
                }
            }
            SessionCommand::Close => {
                self.closed = true;
                if let Some(controller) = &self.controller {
                    controller.cancel().await;
                }
                if let Err(e) = self.context.sessions.forget(self.id) {
                    tracing::warn!("session {}: could not forget it: {e:#}", self.id);
                }
                println!("[{}] closed", self.short());
            }
        }
    }

    /// Takes turns back: the conversation becomes exactly `history`, the
    /// server's flat text (tool calls of the turns kept are lost, as for a
    /// session the host never ran). A turn still running is cancelled and the
    /// live session let go; the next input opens a new one on `history`. The
    /// kept copy is replaced too, so a restart does not bring the old turns
    /// back. Events the cancelled turn had not delivered yet are dropped: they
    /// would otherwise end the answer started next.
    async fn replace_conversation(&mut self, mode: AgentMode, history: Vec<Message>) {
        if let Some(controller) = self.controller.take() {
            controller.cancel().await;
        }
        let (events_tx, events_rx) = mpsc::channel(1024);
        self.events_tx = events_tx;
        self.events_rx = events_rx;
        if self.working {
            self.state(SessionState::Idle).await;
        }
        self.mode = mode;
        self.unfinished = None;
        self.history = history;
        if self.closed {
            return;
        }
        if let Err(e) = self.context.sessions.save(self.id, &self.history) {
            tracing::warn!(
                "session {}: could not keep its replaced conversation: {e:#}",
                self.id
            );
        }
    }

    async fn answer(&mut self, text: String) {
        println!("[{}] > {}", self.short(), first_line(&text));
        // The server asking again for the answer this host was stopped in
        // the middle of: continue it, rather than start it over.
        let text = if self.unfinished.take().as_deref() == Some(text.as_str()) {
            println!("[{}]   continuing the interrupted answer", self.short());
            CONTINUE_INTERRUPTED.to_string()
        } else {
            match slash::interpret(&text, &self.context.working_dir) {
                slash::Typed::Message(text) | slash::Typed::Expanded(text) => text,
                slash::Typed::Mode(mode) => {
                    self.mode = mode;
                    self.reply(&format!("Mode is now {mode}.")).await;
                    return;
                }
                slash::Typed::Usage(message) => {
                    self.reply(&message).await;
                    return;
                }
                slash::Typed::Clear => {
                    self.clear();
                    self.reply("Conversation cleared.").await;
                    return;
                }
                slash::Typed::Compact => {
                    self.compact().await;
                    return;
                }
            }
        };
        if let Err(e) = self.submit(text).await {
            self.fail(&format!("{e:#}")).await;
        }
    }

    /// The model this conversation talks to, through the server.
    fn model_config(&self) -> anyhow::Result<ModelConfig> {
        let name = self
            .model
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no model was chosen for this conversation"))?;
        Ok(ModelConfig {
            provider: "openai".into(),
            name,
            base_url: Some(self.context.openai_base_url.clone()),
            api_key: Some(self.context.token.clone()),
            ..ModelConfig::default()
        })
    }

    /// Opens the live session on the conversation kept so far.
    async fn open_controller(&mut self, model: ModelConfig) -> anyhow::Result<SessionController> {
        let runtime = session_runtime(
            &self.context.config,
            &self.context.working_dir,
            self.context.hosted,
        );
        let mut options = SessionOptions::new(
            Arc::clone(&self.context.config),
            runtime,
            self.events_tx.clone(),
        );
        // A tool that asks first asks the person at the server.
        options.permission_requester = Some(Arc::new(RemoteApprovals {
            session_id: self.id,
            pending: self.approvals.clone(),
            ask: self.context.asks.clone(),
            timeout: APPROVAL_TIMEOUT,
        }));
        let spec = SessionSpec::for_mode(self.mode, model);
        let history = std::mem::take(&mut self.history);
        let (controller, _mcp_events) = SessionController::open(options, spec, history).await?;
        Ok(controller)
    }

    async fn submit(&mut self, text: String) -> anyhow::Result<()> {
        let model = self.model_config()?;
        self.state(SessionState::Running).await;
        match &mut self.controller {
            Some(controller) => {
                controller
                    .submit(text, Some(self.mode), Some(model))
                    .await?
            }
            None => {
                let controller = self.open_controller(model).await?;
                controller.post(text).await?;
                self.controller = Some(controller);
            }
        }
        Ok(())
    }

    /// `/compact`: the session summarises its conversation now. The answer
    /// is the kernel's own: `ContextCompacted` with the summary, or a warning
    /// when there is nothing to summarise, then `TurnComplete`. What the
    /// kernel cannot answer is answered here, so the server never waits for
    /// an ending that will not come: a turn still running (the request is
    /// refused, never queued) and a mode that cannot compact.
    async fn compact(&mut self) {
        if self.working {
            self.reply("Cannot compact now: a turn is running. Stop it or wait for it to finish.")
                .await;
            return;
        }
        if let Err(e) = self.start_compaction().await {
            self.fail(&format!("{e:#}")).await;
        }
    }

    async fn start_compaction(&mut self) -> anyhow::Result<()> {
        if self.controller.is_none() {
            let model = self.model_config()?;
            self.controller = Some(self.open_controller(model).await?);
        }
        self.state(SessionState::Running).await;
        let Some(controller) = &self.controller else {
            return Ok(());
        };
        match controller.compact().await {
            Ok(()) => Ok(()),
            Err(e @ CompactError::Unsupported(_)) => {
                self.reply(&format!("{e}.")).await;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Starts the session over with no conversation: the live one is let go
    /// and its kept copy emptied, so a restart does not bring it back.
    fn clear(&mut self) {
        self.controller = None;
        self.history.clear();
        self.unfinished = None;
        if let Err(e) = self.context.sessions.save(self.id, &[]) {
            tracing::warn!(
                "session {}: could not clear its kept conversation: {e:#}",
                self.id
            );
        }
    }

    /// Answers a command the host carried out itself, ending the answer.
    async fn reply(&mut self, text: &str) {
        println!("[{}]   {text}", self.short());
        self.upload(AgentEvent::TextDelta(text.to_string())).await;
        self.upload(AgentEvent::TurnComplete).await;
        self.state(SessionState::Idle).await;
    }

    /// Reports a failure the session itself never saw, ending the answer.
    async fn fail(&mut self, message: &str) {
        println!("[{}] ! {message}", self.short());
        self.upload(AgentEvent::Error(message.to_string())).await;
        self.upload(AgentEvent::TurnComplete).await;
        self.state(SessionState::Idle).await;
    }

    async fn forward(&mut self, event: AgentEvent) {
        match &event {
            // The kernel's per-dispatch trace: for a debugger, not a reader.
            AgentEvent::Transition { .. } => return,
            AgentEvent::ToolCallStarted(call) => println!("[{}]   {} ...", self.short(), call.name),
            AgentEvent::ToolCallFinished {
                tool_name,
                is_error: true,
                ..
            } => {
                println!("[{}]   {tool_name} failed", self.short());
            }
            AgentEvent::Error(message) => println!("[{}] ! {message}", self.short()),
            _ => {}
        }
        let finished = matches!(event, AgentEvent::TurnComplete | AgentEvent::Aborted { .. });
        // Kept after every finished call too, so a host stopped mid-answer
        // loses none of the work it did.
        let checkpoint = finished || matches!(event, AgentEvent::ToolCallFinished { .. });
        let described = self.describe(&event);
        self.upload(event).await;
        if let Some(described) = described {
            self.send(described).await;
        }
        if checkpoint {
            self.keep();
        }
        if finished {
            println!("[{}] done", self.short());
            self.state(SessionState::Idle).await;
        }
    }

    /// A tool call in words, after it starts and again, timed, after it
    /// finishes (`describe.rs`); nothing for any other event.
    fn describe(&mut self, event: &AgentEvent) -> Option<ControlEvent> {
        let (call_id, described, duration_ms) = match event {
            AgentEvent::ToolCallStarted(call) => {
                let described = describe::describe(&call.name, &call.args);
                self.running
                    .insert(call.id.clone(), (Instant::now(), described.clone()));
                (call.id.clone(), described, None)
            }
            AgentEvent::ToolCallFinished { call_id, .. } => {
                let (started, described) = self.running.remove(call_id)?;
                let ran = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                (call_id.clone(), described, Some(ran))
            }
            _ => return None,
        };
        Some(ControlEvent::ToolCallDescribed {
            session_id: self.id,
            call_id,
            action: described.action,
            title: described.title,
            category: described.category,
            duration_ms,
        })
    }

    async fn send(&self, event: ControlEvent) {
        let _ = self
            .context
            .uploads
            .send(Upload::Event(Box::new(event)))
            .await;
    }

    /// Keeps the session's conversation as it is now.
    fn keep(&self) {
        if self.closed {
            return;
        }
        if let Some(controller) = &self.controller {
            if let Err(e) = self.context.sessions.save(self.id, &controller.history()) {
                tracing::warn!(
                    "session {}: could not keep its conversation: {e:#}",
                    self.id
                );
            }
        }
    }

    async fn upload(&self, event: AgentEvent) {
        let event = ControlEvent::Session {
            session_id: self.id,
            event,
        };
        let _ = self
            .context
            .uploads
            .send(Upload::Event(Box::new(event)))
            .await;
    }

    async fn state(&mut self, state: SessionState) {
        self.working = state == SessionState::Running;
        let event = ControlEvent::SessionState {
            session_id: self.id,
            state,
        };
        let _ = self
            .context
            .uploads
            .send(Upload::Event(Box::new(event)))
            .await;
    }
}

fn first_line(text: &str) -> &str {
    let line = text.lines().next().unwrap_or_default();
    match line.char_indices().nth(100) {
        Some((end, _)) => &line[..end],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_hosted_session_offers_only_tools_that_never_touch_the_machine() {
        let mut config = sven_bootstrap::Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();
        restrict_to_hosted(&mut config);
        assert!(
            config.tools.web.public_only,
            "web_fetch never reaches the server's network"
        );
        let (runtime, handle, ..) =
            sven_bootstrap::RuntimeBuilder::new(std::sync::Arc::new(config), "agent")
                .with_builtin_tools(sven_bootstrap::BuiltinTools::Coding)
                .build()
                .await
                .expect("runtime builds");
        let mut names = handle.tool_registry().names();
        runtime.abort();
        names.sort();
        assert_eq!(
            names,
            ["skill", "web_fetch", "web_search"],
            "every hosted tool is a real one, and nothing else is offered"
        );
    }

    use super::*;
    use sven_control::HistoryRole;

    /// A hosted conversation's system message is the configured prompt, or
    /// sven's hosted default, and nothing else: no project root, sandbox
    /// path, git or tool index, which are the site's internals.
    #[test]
    fn a_hosted_session_is_prompted_with_its_configured_prompt_alone() {
        let dir = std::path::Path::new("/var/lib/whale/hosted/acct/conv/work");
        let mut config = Config::default();
        config.agent.system_prompt = Some("You are a helpful assistant.".into());
        let runtime = session_runtime(&config, dir, true);
        assert!(
            runtime.no_system,
            "the built-in prompt and its context sections are left out"
        );
        assert_eq!(
            runtime.system_prompt_override.as_deref(),
            Some("You are a helpful assistant.")
        );
    }

    #[test]
    fn a_hosted_session_without_a_configured_prompt_gets_one_that_names_no_path() {
        let dir = std::path::Path::new("/var/lib/whale/hosted/acct/conv/work");
        let runtime = session_runtime(&Config::default(), dir, true);
        assert!(runtime.no_system);
        let prompt = runtime
            .system_prompt_override
            .expect("a hosted session always has a prompt");
        assert!(
            !prompt.contains('/'),
            "no path in the hosted default: {prompt}"
        );
    }

    #[test]
    fn a_session_on_a_persons_computer_keeps_its_project_context() {
        let dir = std::path::Path::new("/home/someone/project");
        let mut config = Config::default();
        config.agent.system_prompt = Some("Be terse.".into());
        let runtime = session_runtime(&config, dir, false);
        assert!(
            !runtime.no_system,
            "the project context stays for a local session"
        );
        assert_eq!(runtime.project_root.as_deref(), Some(dir));
        assert_eq!(runtime.system_prompt_override.as_deref(), Some("Be terse."));
    }

    fn turn(role: HistoryRole, text: &str) -> HistoryEntry {
        HistoryEntry {
            role,
            text: text.into(),
        }
    }

    /// A session with a live controller (mock model) holding the original
    /// turns, as after the server's first question and answer.
    async fn running_session(dir: &std::path::Path) -> SessionActor {
        let (uploads, _kept_open) = mpsc::channel(64);
        let (asks, _asks_open) = mpsc::channel(8);
        std::mem::forget(_kept_open);
        std::mem::forget(_asks_open);
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();
        let context = Arc::new(SessionContext {
            config: Arc::new(config),
            working_dir: dir.to_path_buf(),
            hosted: false,
            openai_base_url: String::new(),
            token: String::new(),
            uploads,
            sessions: SessionStore::new(dir.join("kept")),
            asks,
        });
        let mut actor = SessionActor::new(Uuid::new_v4(), context);
        let model = ModelConfig {
            provider: "mock".into(),
            name: "mock-model".into(),
            ..ModelConfig::default()
        };
        actor.history = sven_executors::history_messages([
            turn(HistoryRole::User, "first question"),
            turn(HistoryRole::Assistant, "first answer"),
            turn(HistoryRole::User, "second question"),
            turn(HistoryRole::Assistant, "second answer"),
        ]);
        actor.controller = Some(actor.open_controller(model).await.expect("opens"));
        actor
    }

    /// The conversation's turns; the session's own system prompt is not one.
    fn texts(history: &[Message]) -> Vec<String> {
        history
            .iter()
            .filter(|m| m.role != sven_model::Role::System)
            .map(|m| m.as_text().unwrap_or_default().to_string())
            .collect()
    }

    #[tokio::test]
    async fn replacing_history_leaves_a_running_session_with_exactly_the_given_turns() {
        let dir = tempfile::tempdir().unwrap();
        let mut actor = running_session(dir.path()).await;
        assert_eq!(
            texts(&actor.controller.as_ref().unwrap().history()).len(),
            4,
            "the session starts with the original turns"
        );

        // Regenerate the second answer: the server sends the turns before
        // the question, then the question again as input.
        let replacement = vec![
            turn(HistoryRole::User, "first question"),
            turn(HistoryRole::Assistant, "first answer"),
        ];
        actor
            .handle(SessionCommand::Open {
                mode: AgentMode::Agent,
                history: replacement,
                replace: true,
            })
            .await;

        assert!(actor.controller.is_none(), "the old session is let go");
        assert_eq!(texts(&actor.history), ["first question", "first answer"]);
        let kept = actor.context.sessions.load(actor.id).expect("kept copy");
        assert_eq!(
            texts(&kept),
            ["first question", "first answer"],
            "a restart does not bring the taken-back turns back"
        );

        // The next session opens on exactly that history: the question
        // arrives once, as the input.
        let model = actor.model_config().unwrap_or(ModelConfig {
            provider: "mock".into(),
            name: "mock-model".into(),
            ..ModelConfig::default()
        });
        let reopened = actor.open_controller(model).await.expect("opens");
        assert_eq!(
            texts(&reopened.history()),
            ["first question", "first answer"]
        );
    }

    #[tokio::test]
    async fn opening_again_without_replace_keeps_the_running_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let mut actor = running_session(dir.path()).await;
        actor
            .handle(SessionCommand::Open {
                mode: AgentMode::Agent,
                history: vec![turn(HistoryRole::User, "other")],
                replace: false,
            })
            .await;
        assert_eq!(
            texts(&actor.controller.as_ref().unwrap().history()).len(),
            4
        );
    }

    #[test]
    fn a_long_prompt_prints_as_its_first_line_cut_at_a_character() {
        assert_eq!(first_line("list files\nthen more"), "list files");
        let long = "é".repeat(150);
        assert_eq!(first_line(&long).chars().count(), 100);
    }
}
