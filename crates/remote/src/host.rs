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

use crate::client::{RemoteClient, TokenRefused};
use crate::login::RemoteLogin;
use crate::sessions::{opening_history, SessionStore};

/// Events waiting for the uploader. Full means the server has stopped
/// taking them; sessions then wait rather than grow memory without bound.
const UPLOAD_QUEUE: usize = 4096;
/// The most events one upload carries.
const BATCH_EVENTS: usize = 256;
/// How long the uploader collects before sending what it has.
const BATCH_WINDOW: Duration = Duration::from_millis(150);
/// An empty upload while a session runs: the server's sign that the host
/// is alive and the answer it is waiting for is still coming.
const HEARTBEAT: Duration = Duration::from_secs(10);
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
}

/// Runs until interrupted, or until the server stops accepting the token.
///
/// # Errors
///
/// The token is refused, at start or later.
pub async fn run_host(options: HostOptions) -> anyhow::Result<()> {
    let client = RemoteClient::new(&options.login.control_url, &options.login.token)?;
    let who = client.whoami().await?;
    println!(
        "Connected to {} as \"{}\" for {}. Sessions work in {}. Ctrl-C stops.",
        options.login.server,
        who.workspace_name,
        who.account,
        options.working_dir.display()
    );

    let (uploads, upload_rx) = mpsc::channel(UPLOAD_QUEUE);
    tokio::spawn(upload_loop(client.clone(), upload_rx));
    let context = Arc::new(SessionContext {
        config: options.config,
        working_dir: options.working_dir,
        openai_base_url: options.login.openai_base_url,
        token: options.login.token,
        uploads: uploads.clone(),
        sessions: options.sessions,
    });

    let (routed, routed_rx) = mpsc::channel(SESSION_QUEUE);
    tokio::spawn(router(routed_rx, Arc::clone(&context)));
    tokio::spawn(terminal_loop(routed.clone()));
    println!("Type a message to send it to the conversation in progress here, or @<id> message for another.");

    tokio::select! {
        result = command_loop(&client, &routed, &uploads) => result,
        _ = tokio::signal::ctrl_c() => {
            println!("Stopping. Sessions in progress end here.");
            Ok(())
        }
    }
}

async fn command_loop(
    client: &RemoteClient,
    routed: &mpsc::Sender<Routed>,
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
                    let _ = routed.send(Routed::Command(envelope.command)).await;
                    let _ = uploads.send(Upload::Ack(taken)).await;
                }
            }
            Err(e) if e.downcast_ref::<TokenRefused>().is_some() => return Err(e),
            Err(e) => tracing::warn!("command stream: {e:#}"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// What reaches the router: a command from the server, or a line typed at
/// this host's terminal.
enum Routed {
    Command(ControlCommand),
    Terminal(String),
}

/// Owns the sessions: starts each one's actor on first sight and hands it its
/// commands, and sends a terminal line to the session it names (`@<id
/// prefix> text`) or else to the one most recently given input.
async fn router(mut rx: mpsc::Receiver<Routed>, context: Arc<SessionContext>) {
    let mut sessions: HashMap<Uuid, mpsc::Sender<SessionCommand>> = HashMap::new();
    let mut last_active: Option<Uuid> = None;
    while let Some(routed) = rx.recv().await {
        match routed {
            Routed::Command(command) => {
                if let ControlCommand::SendInput { session_id, .. } = &command {
                    last_active = Some(*session_id);
                }
                route(command, &mut sessions, &context).await;
            }
            Routed::Terminal(line) => {
                let Some((target, text)) = terminal_target(&line, sessions.keys(), last_active)
                else {
                    println!("No conversation to send that to yet: start one from the server, or name one with @<id>.");
                    continue;
                };
                last_active = Some(target);
                if let Some(actor) = sessions.get(&target) {
                    let _ = actor.send(SessionCommand::Terminal(text)).await;
                }
            }
        }
    }
}

/// The session a terminal line is for and its text: `@<prefix> text` names a
/// session by the start of its id (one match only), anything else goes to
/// `last_active`. None when there is no such session or nothing to send.
fn terminal_target<'a>(
    line: &str,
    known: impl Iterator<Item = &'a Uuid>,
    last_active: Option<Uuid>,
) -> Option<(Uuid, String)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(rest) = line.strip_prefix('@') {
        let (prefix, text) = rest.split_once(char::is_whitespace)?;
        let matches: Vec<Uuid> = known
            .filter(|id| id.to_string().starts_with(prefix))
            .copied()
            .collect();
        let text = text.trim();
        return match matches.as_slice() {
            [only] if !text.is_empty() => Some((*only, text.to_string())),
            _ => None,
        };
    }
    last_active.map(|id| (id, line.to_string()))
}

/// Lines typed at this host's terminal, until it closes (a host started with
/// no terminal reads nothing and carries on).
async fn terminal_loop(routed: mpsc::Sender<Routed>) {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if routed.send(Routed::Terminal(line)).await.is_err() {
            break;
        }
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

/// What every session shares.
struct SessionContext {
    config: Arc<Config>,
    working_dir: PathBuf,
    openai_base_url: String,
    token: String,
    uploads: mpsc::Sender<Upload>,
    /// Where each session's own conversation is kept between turns.
    sessions: SessionStore,
}

enum SessionCommand {
    Open {
        mode: AgentMode,
        history: Vec<HistoryEntry>,
    },
    Model(String),
    Input(String),
    /// A line typed at this host's terminal for this session.
    Terminal(String),
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
    controller: Option<SessionController>,
    /// An answer is being written; terminal input waits for it.
    answering: bool,
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
            controller: None,
            answering: false,
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
                    self.history = opening_history(
                        self.context.sessions.load(self.id),
                        sven_executors::history_messages(history),
                    );
                }
            }
            SessionCommand::Model(model) => self.model = Some(model),
            SessionCommand::Input(text) => self.answer(text).await,
            SessionCommand::Terminal(text) => {
                if self.answering {
                    println!(
                        "[{}] still answering; send it when the answer is done",
                        self.short()
                    );
                    return;
                }
                // The server records the message before the answer's events.
                let input = ControlEvent::UserInput {
                    session_id: self.id,
                    text: text.clone(),
                };
                let _ = self
                    .context
                    .uploads
                    .send(Upload::Event(Box::new(input)))
                    .await;
                self.answer(text).await;
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
        self.answering = true;
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
                let options = SessionOptions::new(
                    Arc::clone(&self.context.config),
                    runtime,
                    self.events_tx.clone(),
                );
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

    /// Reports a failure the session itself never saw, ending the answer.
    async fn fail(&mut self, message: &str) {
        println!("[{}] ! {message}", self.short());
        self.answering = false;
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
        self.upload(event).await;
        if finished {
            self.answering = false;
            println!("[{}] done", self.short());
            if let Some(controller) = &self.controller {
                if let Err(e) = self.context.sessions.save(self.id, &controller.history()) {
                    tracing::warn!(
                        "session {}: could not keep its conversation: {e:#}",
                        self.id
                    );
                }
            }
            self.state(SessionState::Idle).await;
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
async fn upload_loop(client: RemoteClient, mut rx: mpsc::Receiver<Upload>) {
    // Numbered from 1 within this run; the server tells runs apart by `run`,
    // so a restarted host is never taken for one replaying old events.
    let run = Uuid::new_v4();
    let mut next_seq = 1u64;
    let mut running: HashMap<Uuid, bool> = HashMap::new();
    let mut last_upload = Instant::now();
    loop {
        let mut batch = EventBatch {
            run,
            events: Vec::new(),
            acked_through: None,
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
        let heartbeat_due = running.values().any(|on| *on) && last_upload.elapsed() >= HEARTBEAT;
        if batch.events.is_empty() && batch.acked_through.is_none() && !heartbeat_due {
            continue;
        }
        let mut backoff = Duration::from_millis(500);
        while let Err(e) = client.post_events(&batch).await {
            tracing::warn!("event upload: {e:#}");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
        last_upload = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_line_goes_to_the_session_it_names_or_else_the_last_active_one() {
        let a = Uuid::parse_str("aaaa1111-0000-4000-8000-000000000000").unwrap();
        let b = Uuid::parse_str("bbbb2222-0000-4000-8000-000000000000").unwrap();
        let known = [a, b];
        assert_eq!(
            terminal_target("hello", known.iter(), Some(a)),
            Some((a, "hello".into()))
        );
        assert_eq!(
            terminal_target("@bbbb  run the tests ", known.iter(), Some(a)),
            Some((b, "run the tests".into()))
        );
        assert_eq!(
            terminal_target("hello", known.iter(), None),
            None,
            "no conversation yet"
        );
        assert_eq!(
            terminal_target("@cccc hi", known.iter(), Some(a)),
            None,
            "no such session"
        );
        assert_eq!(
            terminal_target("@ hi", known.iter(), Some(a)),
            None,
            "ambiguous: every id matches"
        );
        assert_eq!(
            terminal_target("@aaaa", known.iter(), Some(a)),
            None,
            "nothing to send"
        );
        assert_eq!(terminal_target("   ", known.iter(), Some(a)), None);
    }

    #[test]
    fn a_long_prompt_prints_as_its_first_line_cut_at_a_character() {
        assert_eq!(first_line("list files\nthen more"), "list files");
        let long = "é".repeat(150);
        assert_eq!(first_line(&long).chars().count(), 100);
    }
}
