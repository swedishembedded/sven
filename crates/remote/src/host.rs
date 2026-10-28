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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use sven_bootstrap::{Config, RuntimeContext};
use sven_control::remote::{EventBatch, EventEnvelope};
use sven_control::{ControlCommand, ControlEvent, HistoryEntry, SessionState};
use sven_frontend::{SessionController, SessionOptions, SessionSpec};
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

mod slash;

/// Events waiting for the uploader. Full means the server has stopped
/// taking them; sessions then wait rather than grow memory without bound.
const UPLOAD_QUEUE: usize = 4096;
/// The most events one upload carries.
const BATCH_EVENTS: usize = 256;
/// How long the uploader collects before sending what it has.
const BATCH_WINDOW: Duration = Duration::from_millis(150);
/// The longest the host goes without an upload: the server's sign that it is
/// alive (and, with `working`, which answers are still coming). Rare, so
/// thousands of idle hosts cost the server little; anything the host has to
/// say (a command it took, an event) goes at once and counts too.
const HEARTBEAT: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Commands queued for one session while it works.
const SESSION_QUEUE: usize = 64;

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
            id, mode, history, ..
        } => (id, SessionCommand::Open { mode, history }),
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
    },
    Model(String),
    Input(String),
    /// The server's answer to a tool call waiting for permission.
    Decide {
        call_id: String,
        approved: bool,
    },
    Cancel,
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
    /// This session's tool calls waiting for the server's permission.
    approvals: PendingApprovals,
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
            approvals: PendingApprovals::default(),
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
            SessionCommand::Open { mode, history } => {
                // Idempotent: a running session keeps its own conversation.
                if self.controller.is_none() {
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
            }
        };
        if let Err(e) = self.submit(text).await {
            self.fail(&format!("{e:#}")).await;
        }
    }

    async fn submit(&mut self, text: String) -> anyhow::Result<()> {
        let name = self
            .model
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no model was chosen for this conversation"))?;
        let model = ModelConfig {
            provider: "openai".into(),
            name,
            base_url: Some(self.context.openai_base_url.clone()),
            api_key: Some(self.context.token.clone()),
            ..ModelConfig::default()
        };
        self.state(SessionState::Running).await;
        match &mut self.controller {
            Some(controller) => {
                controller
                    .submit(text, Some(self.mode), Some(model))
                    .await?
            }
            None => {
                let mut runtime =
                    RuntimeContext::auto_detect_at(Some(self.context.working_dir.clone()));
                runtime.project_root = Some(self.context.working_dir.clone());
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
                let (controller, _mcp_events) =
                    SessionController::open(options, spec, history).await?;
                controller.post(text).await?;
                self.controller = Some(controller);
            }
        }
        Ok(())
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
        self.upload(event).await;
        if checkpoint {
            self.keep();
        }
        if finished {
            println!("[{}] done", self.short());
            self.state(SessionState::Idle).await;
        }
    }

    /// Keeps the session's conversation as it is now.
    fn keep(&self) {
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

    async fn state(&self, state: SessionState) {
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

enum Upload {
    Event(Box<ControlEvent>),
    /// The highest command `seq` taken.
    Ack(u64),
}

/// Numbers, batches and uploads events in order. A batch is sent again until
/// the server stores it, so nothing is lost while the server is away; the
/// server drops a `(run, seq)` it already has, so nothing is stored twice.
async fn upload_loop(client: RemoteClient, mut rx: mpsc::Receiver<Upload>, run: Uuid) {
    // Numbered from 1 within this run; the server tells runs apart by `run`,
    // so a restarted host is never taken for one replaying old events.
    let mut next_seq = 1u64;
    let mut running: HashMap<Uuid, bool> = HashMap::new();
    let mut last_upload = Instant::now();
    loop {
        let mut batch = EventBatch {
            run,
            events: Vec::new(),
            acked_through: None,
            working: Vec::new(),
        };
        let first = tokio::time::timeout(HEARTBEAT, rx.recv()).await;
        let mut take = |upload: Upload, batch: &mut EventBatch| match upload {
            Upload::Ack(seq) => batch.acked_through = Some(seq),
            Upload::Event(event) => {
                let event = *event;
                if let ControlEvent::SessionState { session_id, state } = &event {
                    running.insert(*session_id, *state == SessionState::Running);
                }
                batch.events.push(EventEnvelope {
                    seq: next_seq,
                    event,
                });
                next_seq += 1;
            }
        };
        match first {
            Ok(Some(upload)) => take(upload, &mut batch),
            Ok(None) => return,
            Err(_) => {}
        }
        let window = tokio::time::sleep(BATCH_WINDOW);
        tokio::pin!(window);
        while batch.events.len() < BATCH_EVENTS && !batch.events.is_empty() {
            tokio::select! {
                upload = rx.recv() => match upload {
                    Some(upload) => take(upload, &mut batch),
                    None => break,
                },
                () = &mut window => break,
            }
        }
        // An upload at least every HEARTBEAT, idle or not, is how the server
        // knows this host is alive: a host that froze or lost its network
        // stops sending them, and its environment is let go.
        let heartbeat_due = last_upload.elapsed() >= HEARTBEAT;
        if batch.events.is_empty() && batch.acked_through.is_none() && !heartbeat_due {
            continue;
        }
        batch.working = running
            .iter()
            .filter(|(_, on)| **on)
            .map(|(id, _)| *id)
            .collect();
        let mut backoff = Duration::from_millis(500);
        while let Err(e) = client.post_events(&batch).await {
            // Replaced: nothing more from this run is taken; the command
            // stream is refused too, and that stops the host.
            if e.downcast_ref::<EnvironmentTaken>().is_some() {
                tracing::warn!("event upload: {e:#}");
                return;
            }
            tracing::warn!("event upload: {e:#}");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
        last_upload = Instant::now();
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

    #[test]
    fn a_long_prompt_prints_as_its_first_line_cut_at_a_character() {
        assert_eq!(first_line("list files\nthen more"), "list files");
        let long = "é".repeat(150);
        assert_eq!(first_line(&long).chars().count(), 100);
    }
}
