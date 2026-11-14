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

use crate::printable::printable;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sven_bootstrap::Config;
use sven_control::{ControlEvent, HistoryEntry, SessionState};
use sven_frontend::SessionController;
use sven_machines::AgentEvent;
use sven_model::Message;
use sven_tool_api::Permission;
use sven_vocab::goal::Goal;
use sven_vocab::AgentMode;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::approvals::{PendingApprovals, Turn, TurnAuthority};
use crate::client::RemoteClient;
use crate::login::RemoteLogin;
use crate::sessions::{opening, SessionStore, CONTINUE_INTERRUPTED};
use answers::{sleep_until, Answers};
use upload::{upload_loop, Upload};

mod answers;
mod attachments;
mod commands;
mod conversation_files;
mod describe;
mod hosted;
mod inbox;
mod opening;
mod router;
mod services;
mod slash;
mod steering;
mod upload;

pub use hosted::{restrict_to_hosted, restrict_to_working_dir, HOSTED_SYSTEM_PROMPT, HOSTED_TOOLS};
use inbox::command_loop;
use router::{list_slash_commands, router};
use services::use_server_services;

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
    /// The person chose to let the server's sessions reach beyond the
    /// working directory and the public internet (`sven rc --wide-access`):
    /// otherwise [`restrict_to_working_dir`] applies.
    pub wide_access: bool,
    /// The working directory serves several conversations (a hosted host in
    /// a sandbox, one per person): each then works in a directory of its own
    /// below it, held there, and removed with it. A container serves one
    /// conversation and a person's computer is theirs: neither is split.
    pub shared_workdir: bool,
}

/// Runs until interrupted, or until the server stops accepting the token.
///
/// # Errors
///
/// The token is refused, at start or later.
pub async fn run_host(options: HostOptions) -> anyhow::Result<()> {
    // The sign-in key is in this process's memory; its tool children are not
    // to read it from there.
    crate::sandbox::keep_memory_private()?;
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
    // Searches go to this server, which runs and bills them.
    let mut config = (*options.config).clone();
    if !options.hosted && !options.wide_access {
        restrict_to_working_dir(&mut config);
    }
    use_server_services(&mut config, &options.login);
    let context = Arc::new(SessionContext {
        config: Arc::new(config),
        working_dir: options.working_dir,
        hosted: options.hosted,
        wide_access: options.wide_access,
        shared_workdir: options.shared_workdir,
        openai_base_url: options.login.openai_base_url,
        token: options.login.token,
        client: client.clone(),
        uploads: uploads.clone(),
        sessions: options.sessions,
        asks,
        mcp_tools_wait_ms: MCP_TOOLS_WAIT_MS,
    });

    // What a session here can run, so the server can offer it as it is typed.
    list_slash_commands(&context);

    let (routed, routed_rx) = mpsc::channel(SESSION_QUEUE);
    tokio::spawn(router(routed_rx, Arc::clone(&context)));
    println!("Waiting for conversations. To write to one, run `sven rc attach <id>`.");

    tokio::select! {
        result = command_loop(&client, &routed, &uploads) => result,
        () = crate::stop::requested() => {
            println!("Stopping. Sessions in progress end here.");
            if let Err(e) = client.detach_environment().await {
                tracing::warn!("could not tell the server this host stopped: {e:#}");
            }
            Ok(())
        }
    }
}

/// What every session shares.
struct SessionContext {
    config: Arc<Config>,
    working_dir: PathBuf,
    /// Run by a site for its own web conversations (`sven rc --hosted`).
    hosted: bool,
    /// See [`HostOptions::wide_access`].
    wide_access: bool,
    /// See [`HostOptions::shared_workdir`].
    shared_workdir: bool,
    openai_base_url: String,
    token: String,
    /// The server, for the files a session's tools publish to its
    /// conversation.
    client: RemoteClient,
    uploads: mpsc::Sender<Upload>,
    /// Where each session's own conversation is kept between turns.
    sessions: SessionStore,
    /// Where a session's tool calls that need permission are asked about.
    asks: mpsc::Sender<ControlEvent>,
    /// How long a session opening waits for its MCP servers' tools.
    mcp_tools_wait_ms: u64,
}

/// How long a session opening waits for its MCP servers to list their tools
/// (the connectors server reads each connected account's list from its
/// service), so the first answer can already use them. A server slower than
/// this still reaches the session once it lists them.
const MCP_TOOLS_WAIT_MS: u64 = 20_000;

enum SessionCommand {
    Open {
        mode: AgentMode,
        history: Vec<HistoryEntry>,
        /// The server took turns back: `history` replaces the conversation
        /// of a session already running, instead of being ignored.
        replace: bool,
    },
    Model(String),
    Input(answers::Input),
    /// The server tells the conversation something (`notify`).
    Notice(sven_vocab::Notice, bool),
    /// Let go of the session to free what it holds: its record is saved and
    /// the actor ends; the next command for it starts it again from that
    /// record (`router::release_idle`). Sent only while no turn runs.
    Release,
    /// The server's answer to a tool call waiting for permission.
    Decide {
        call_id: String,
        answer: Permission,
    },
    Cancel,
    /// The server asks how the session's goal stands.
    ReportGoal,
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
    /// The session's standing goal as it last stood, kept beside the
    /// conversation so a restarted host gives it back to the session.
    goal: Option<Goal>,
    controller: Option<SessionController>,
    /// A turn or a compaction is running: the last state told to the server
    /// was `Running`.
    working: bool,
    /// Closed: nothing is kept for it any more, even by an answer that was
    /// still finishing.
    closed: bool,
    /// Released by the router: its record is saved and the actor ends.
    released: bool,
    /// What the router reads to decide when to release it.
    activity: Arc<router::Activity>,
    /// This session's tool calls waiting for the server's permission.
    approvals: PendingApprovals,
    /// The turn running: its answer, and whether it asks about every call.
    turn: TurnAuthority,
    /// Whether the input being handled asks about every call: someone other
    /// than the conversation's owner sent it.
    asking_every_tool: bool,
    /// Each running tool call's description and when it started, to tell
    /// the server how long it ran.
    running: HashMap<String, (Instant, describe::Described)>,
    /// The answers waiting for the session's ending (`answers.rs`).
    answers: Answers,
    /// Notices that arrived while a turn ran, told when it ends
    /// (`commands.rs`).
    held_notices: std::collections::VecDeque<(sven_vocab::Notice, bool)>,
    /// The answer of the input being handled, and whether the session was
    /// asked to end it (else the host ends it itself).
    asking: Option<String>,
    expecting: bool,
    /// Whether the input being handled steers the running turn.
    steering_input: bool,
    /// Messages sent to steer the running turn that it has not taken yet,
    /// oldest first: a `Steered` takes them all; the turn's end leaves them
    /// to be answered next.
    steering: std::collections::VecDeque<Steer>,
    /// Messages sent while a turn ran, each answered in order once the
    /// turn before it ends ([`Self::answer_queued`]).
    queued: std::collections::VecDeque<Steer>,
    /// The model a test scripts in place of the server's.
    #[cfg(test)]
    scripted_model: Option<sven_model_drivers::ModelConfig>,
    events_tx: mpsc::Sender<AgentEvent>,
    events_rx: mpsc::Receiver<AgentEvent>,
}

/// A message sent to steer the running turn, and the answer it is shown as.
#[derive(Debug)]
struct Steer {
    answer_id: Option<String>,
    /// Its turn asks about every call ([`Turn::ask_every_tool`]).
    ask_every_tool: bool,
    input: attachments::Attached,
}

impl SessionActor {
    fn new(id: Uuid, context: Arc<SessionContext>, activity: Arc<router::Activity>) -> Self {
        let (events_tx, events_rx) = mpsc::channel(1024);
        Self {
            id,
            context,
            mode: AgentMode::Agent,
            model: None,
            history: Vec::new(),
            unfinished: None,
            goal: None,
            controller: None,
            working: false,
            closed: false,
            released: false,
            activity,
            approvals: PendingApprovals::default(),
            turn: TurnAuthority::default(),
            asking_every_tool: false,
            running: HashMap::new(),
            answers: Answers::default(),
            held_notices: std::collections::VecDeque::new(),
            asking: None,
            expecting: false,
            steering_input: false,
            steering: std::collections::VecDeque::new(),
            queued: std::collections::VecDeque::new(),
            #[cfg(test)]
            scripted_model: None,
            events_tx,
            events_rx,
        }
    }

    fn short(&self) -> String {
        self.id.to_string()[..8].to_string()
    }

    async fn run(mut self, mut commands: mpsc::Receiver<SessionCommand>) {
        loop {
            let deadline = self.answers.deadline();
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => {
                        self.handle(command).await;
                        if self.released {
                            break;
                        }
                    }
                    None => break,
                },
                Some(event) = self.events_rx.recv() => self.forward(event).await,
                () = sleep_until(deadline), if deadline.is_some() => {
                    tracing::debug!("session {}: a stopped answer never ended", self.id);
                    self.answers.give_up();
                    self.release_held().await;
                }
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
                    self.goal = self.context.sessions.load_goal(self.id);
                }
            }
            SessionCommand::Model(model) => self.model = Some(model),
            SessionCommand::Input(input) => {
                if let Some(input) = self.answers.admit(input, Instant::now()) {
                    self.take(input).await;
                }
            }
            SessionCommand::Notice(notice, start_turn) => self.notice(notice, start_turn).await,
            SessionCommand::Decide { call_id, answer } => {
                if !self.approvals.decide(&call_id, answer) {
                    tracing::debug!(
                        "session {}: no call {call_id} waits for permission",
                        self.id
                    );
                }
            }
            SessionCommand::Cancel => {
                if self.working {
                    self.answers.stop();
                }
                if let Some(controller) = &self.controller {
                    controller.cancel().await;
                }
            }
            SessionCommand::ReportGoal => {
                let goal = self.current_goal();
                self.send(ControlEvent::Goal {
                    session_id: self.id,
                    goal: goal.map(Box::new),
                })
                .await;
            }
            SessionCommand::Release => {
                // Never while a turn runs: the router releases only an idle
                // session, and one that became busy since keeps going.
                if self.working {
                    return;
                }
                self.keep();
                self.released = true;
            }
            SessionCommand::Close => {
                self.closed = true;
                if let Some(controller) = &self.controller {
                    controller.cancel().await;
                }
                if let Err(e) = self.context.sessions.forget(self.id) {
                    tracing::warn!("session {}: could not forget it: {e:#}", self.id);
                }
                hosted::forget_conversation_files(
                    &self.context.working_dir,
                    self.context.shared_workdir,
                    self.id,
                );
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
        self.answers.forget();
        self.held_notices.clear();
        // The turns they waited behind are taken back with the conversation:
        // their answers end, empty.
        for queued in std::mem::take(&mut self.queued) {
            self.upload(AgentEvent::TurnComplete, queued.answer_id)
                .await;
        }
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

    async fn answer(&mut self, text: String, attached: &[sven_vocab::ArtifactRef]) {
        println!("[{}] > {}", self.short(), printable(first_line(&text)));
        // Every client keeps to this limit; a message over it never reaches the model.
        if !sven_vocab::message::fits_message(&text) {
            return self.fail(&sven_vocab::message::too_long(text.len())).await;
        }
        // The server asking again for the answer this host was stopped in
        // the middle of: continue it, rather than start it over.
        let input = if self.unfinished.take().as_deref() == Some(text.as_str()) {
            println!("[{}]   continuing the interrupted answer", self.short());
            attachments::Attached {
                text: CONTINUE_INTERRUPTED.to_string(),
                ..attachments::Attached::default()
            }
        } else {
            match slash::interpret(&text, &self.context.working_dir) {
                slash::Typed::Message(text) | slash::Typed::Expanded(text) => {
                    match self.with_attachments(text, attached).await {
                        Ok(input) => input,
                        Err(e) => {
                            self.fail(&format!("{e:#}")).await;
                            return;
                        }
                    }
                }
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
                slash::Typed::Goal(command) => {
                    self.goal_command(command).await;
                    return;
                }
                slash::Typed::ShowGoal => {
                    let text = sven_frontend::goal::describe(self.goal.as_ref());
                    self.reply(&text).await;
                    return;
                }
            }
        };
        if self.working {
            // A message steers the running turn only when it carries the same
            // authority: one someone else sent never rides on the owner's
            // turn, nor the owner's on theirs. Otherwise it waits its turn.
            let same_authority = self.turn.get().ask_every_tool == self.asking_every_tool;
            if self.steering_input && same_authority && self.steer(&input).await {
                println!("[{}]   steering the running turn", self.short());
                self.steering.push_back(Steer {
                    answer_id: self.asking.clone(),
                    ask_every_tool: self.asking_every_tool,
                    input,
                });
                return;
            }
            // The session takes no message while its turn runs (one being
            // stopped may still be winding a long tool down): it waits, and
            // is answered as soon as the turn ends, never refused.
            println!("[{}]   queued behind the running turn", self.short());
            self.queued.push_back(Steer {
                answer_id: self.asking.clone(),
                ask_every_tool: self.asking_every_tool,
                input,
            });
            return;
        }
        self.start_turn(self.asking.clone(), self.asking_every_tool);
        if let Err(e) = self.submit(input).await {
            self.fail(&format!("{e:#}")).await;
        }
    }

    /// Posts the person's input: its text, and with it the images the model
    /// takes, as one message.
    async fn submit(&mut self, input: attachments::Attached) -> anyhow::Result<()> {
        let model = self.model_config()?;
        self.state(SessionState::Running).await;
        self.expect_ending();
        if self.controller.is_none() && !input.images.is_empty() {
            self.controller = Some(self.open_controller(model.clone()).await?);
        }
        match &mut self.controller {
            Some(controller) if !input.images.is_empty() => {
                let mut parts = vec![sven_model::ContentPart::text(input.text)];
                parts.extend(input.images);
                controller
                    .submit_parts(parts, Some(self.mode), Some(model))
                    .await?
            }
            Some(controller) => {
                controller
                    .submit(input.text, Some(self.mode), Some(model))
                    .await?
            }
            None => {
                let controller = self.open_controller(model).await?;
                controller.post(input.text).await?;
                self.controller = Some(controller);
            }
        }
        Ok(())
    }

    /// Starts the session over with no conversation: the live one is let go
    /// and its kept copy emptied, so a restart does not bring it back. Its
    /// goal goes with it.
    fn clear(&mut self) {
        self.controller = None;
        self.held_notices.clear();
        self.history.clear();
        self.unfinished = None;
        self.keep_goal(None);
        if let Err(e) = self.context.sessions.save(self.id, &[]) {
            tracing::warn!(
                "session {}: could not clear its kept conversation: {e:#}",
                self.id
            );
        }
    }

    /// Takes one input of the server's, as the answer it names.
    async fn take(&mut self, input: answers::Input) {
        self.asking = input.answer_id;
        self.asking_every_tool = input.ask_every_tool;
        self.expecting = false;
        self.steering_input = input.steer;
        self.answer(input.text, &input.attachments).await;
        self.steering_input = false;
        self.asking = None;
        self.asking_every_tool = false;
    }

    /// A turn starts, as the answer `answer_id`: what its calls are judged by.
    fn start_turn(&self, answer_id: Option<String>, ask_every_tool: bool) {
        self.turn.set(Turn {
            answer_id,
            ask_every_tool,
        });
    }

    /// The session is asked for the answer of the input being handled.
    fn expect_ending(&mut self) {
        self.answers.expect(self.asking.clone());
        self.expecting = true;
    }

    /// Answers a command the host carried out itself, ending the answer.
    async fn reply(&mut self, text: &str) {
        println!("[{}]   {}", self.short(), printable(text));
        self.end_asked(AgentEvent::TextDelta(text.to_string()))
            .await;
    }

    /// Reports a failure the session itself never saw, ending the answer.
    async fn fail(&mut self, message: &str) {
        println!("[{}] ! {}", self.short(), printable(message));
        self.end_asked(AgentEvent::Error(message.to_string())).await;
    }

    /// Ends the answer of the input being handled with `last`; the session,
    /// if it was asked for it, no longer ends it. The state goes back to idle
    /// only when no other answer is being written.
    async fn end_asked(&mut self, last: AgentEvent) {
        if std::mem::take(&mut self.expecting) {
            self.answers.withdraw();
        }
        let id = self.asking.clone();
        self.upload(last, id.clone()).await;
        self.upload(AgentEvent::TurnComplete, id).await;
        if self.answers.writing() {
            return;
        }
        self.state(SessionState::Idle).await;
    }

    async fn forward(&mut self, event: AgentEvent) {
        match &event {
            // The kernel's per-dispatch trace: for a debugger, not a reader.
            AgentEvent::Transition { .. } => return,
            AgentEvent::ToolCallStarted(call) => {
                println!("[{}]   {} ...", self.short(), printable(&call.name));
            }
            AgentEvent::ToolCallFinished {
                tool_name,
                is_error: true,
                ..
            } => {
                println!("[{}]   {} failed", self.short(), printable(tool_name));
            }
            AgentEvent::Error(message) => println!("[{}] ! {}", self.short(), printable(message)),
            AgentEvent::GoalUpdated(update) => {
                println!("[{}]   goal: {}", self.short(), printable(&update.note));
                self.keep_goal(update.goal.clone());
            }
            _ => {}
        }
        let finished = matches!(event, AgentEvent::TurnComplete | AgentEvent::Aborted { .. });
        // Kept after every finished call too, so a host stopped mid-answer
        // loses none of the work it did.
        let checkpoint = finished
            || matches!(
                event,
                AgentEvent::ToolCallFinished { .. } | AgentEvent::Notice(_)
            );
        if matches!(event, AgentEvent::Steered { .. }) {
            self.steered().await;
        }
        let stopped = matches!(event, AgentEvent::Aborted { .. });
        let described = self.describe_call(&event);
        self.upload(event, self.answers.current()).await;
        if let Some(described) = described {
            self.send(described).await;
        }
        if checkpoint {
            self.keep();
        }
        if finished {
            println!("[{}] done", self.short());
            self.answers.ended();
            self.state(SessionState::Idle).await;
            self.steer_left_over(stopped).await;
            self.answer_queued().await;
            self.release_held().await;
            self.tell_held_notices().await;
        }
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

    async fn upload(&self, event: AgentEvent, answer_id: Option<String>) {
        let event = ControlEvent::Session {
            session_id: self.id,
            event,
            answer_id,
        };
        let _ = self
            .context
            .uploads
            .send(Upload::Event(Box::new(event)))
            .await;
    }

    async fn state(&mut self, state: SessionState) {
        self.working = state == SessionState::Running;
        self.activity.set_working(self.working);
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
        // The document tools read documents only in confined worker
        // processes (`ocr.isolate`), never in the host.
        assert_eq!(
            names,
            [
                "document_convert",
                "document_locate",
                "skill",
                "task",
                "web_fetch",
                "web_search"
            ],
            "every hosted tool is a real one, and nothing else is offered"
        );
    }

    /// As a hosted host really runs a session: its tool set detected and its
    /// server's services added. It runs other models there, and nothing more.
    #[tokio::test]
    async fn a_hosted_session_runs_models_at_its_server() {
        let mut config = sven_bootstrap::Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();
        restrict_to_hosted(&mut config);
        let login = RemoteLogin {
            server: "https://sven.example".into(),
            control_url: "https://sven.example/api/control".into(),
            openai_base_url: "https://sven.example/v1".into(),
            mcp_url: None,
            mcp_token_url: None,
            transfer_origins: Vec::new(),
            token: "server_minted_key".into(),
            workspace_id: Uuid::nil(),
            workspace_name: "Hosted".into(),
            account: "a@example.com".into(),
        };
        use_server_services(&mut config, &login);
        let (runtime, handle, ..) =
            sven_bootstrap::RuntimeBuilder::new(std::sync::Arc::new(config), "agent")
                .build()
                .await
                .expect("runtime builds");
        let mut names = handle.tool_registry().names();
        runtime.abort();
        names.sort();
        assert_eq!(
            names,
            [
                "document_convert",
                "document_locate",
                "model_schema",
                "models",
                "run_model",
                "skill",
                "task",
                "web_fetch",
                "web_search"
            ]
        );
    }

    use super::hosted::session_runtime;
    use super::*;
    use sven_control::HistoryRole;
    use sven_model_drivers::ModelConfig;

    /// A host searches through the server it signed in to, with its key:
    /// the server runs the search and bills that account.
    #[test]
    fn a_signed_in_host_searches_through_its_server() {
        let mut config = sven_bootstrap::Config::default();
        config.tools.web.search.api_key = Some("brave-key".into());
        let login = RemoteLogin {
            server: "https://sven.example".into(),
            control_url: "https://sven.example/api/control".into(),
            openai_base_url: "https://sven.example/v1".into(),
            mcp_url: None,
            mcp_token_url: None,
            transfer_origins: Vec::new(),
            token: "server_minted_key".into(),
            workspace_id: Uuid::nil(),
            workspace_name: "Workspace".into(),
            account: "a@example.com".into(),
        };
        config.tools.web.search.sven = Some(login.search_endpoint());
        assert_eq!(
            config.tools.web.search_tool(None).backend,
            sven_tools_web::SearchBackend::Sven(sven_tools_web::SvenEndpoint {
                base_url: "https://sven.example/v1".into(),
                api_key: "server_minted_key".into(),
            })
        );
    }

    /// A hosted conversation's system message is the configured prompt, or
    /// sven's hosted default, and nothing else: no project root, sandbox
    /// path, git or tool index, which are the site's internals.
    #[test]
    fn a_hosted_session_is_prompted_with_its_configured_prompt_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.agent.system_prompt = Some("You are a helpful assistant.".into());
        let runtime = session_runtime(&config, dir.path(), true, false, true).unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let runtime = session_runtime(&Config::default(), dir.path(), true, false, true).unwrap();
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
        let dir = std::path::Path::new("someone/project");
        let mut config = Config::default();
        config.agent.system_prompt = Some("Be terse.".into());
        let runtime = session_runtime(&config, dir, false, true, false).unwrap();
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
            attachments: Vec::new(),
        }
    }

    /// A session with a live controller (mock model) holding the original
    /// turns, as after the server's first question and answer.
    async fn running_session(dir: &std::path::Path) -> SessionActor {
        let mut actor = idle_session(dir, mock_config(), 0);
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

    fn mock_config() -> Config {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();
        config
    }

    /// A session of this host not opened yet, with `config`, whose opening
    /// waits `mcp_tools_wait_ms` for its MCP servers' tools.
    #[test]
    fn its_model_calls_name_the_conversation_so_the_server_records_their_cost() {
        let dir = tempfile::tempdir().unwrap();
        let mut actor = idle_session(dir.path(), Config::default(), 0);
        actor.model = Some("e2e/agent".into());
        let model = actor.model_config().expect("a model was chosen");
        assert_eq!(model.session, Some(actor.id.to_string()));
    }

    fn idle_session(dir: &std::path::Path, config: Config, mcp_tools_wait_ms: u64) -> SessionActor {
        let (uploads, _kept_open) = mpsc::channel(64);
        let (asks, _asks_open) = mpsc::channel(8);
        std::mem::forget(_kept_open);
        std::mem::forget(_asks_open);
        let context = Arc::new(SessionContext {
            config: Arc::new(config),
            working_dir: dir.to_path_buf(),
            hosted: false,
            wide_access: true,
            shared_workdir: false,
            openai_base_url: String::new(),
            token: String::new(),
            client: crate::client::RemoteClient::new("http://127.0.0.1:9/api", "token").unwrap(),
            uploads,
            sessions: SessionStore::new(dir.join("kept")),
            asks,
            mcp_tools_wait_ms,
        });
        SessionActor::new(Uuid::new_v4(), context, Arc::default())
    }

    /// An MCP server over Streamable HTTP with one tool, `ping`, that lists
    /// it only after `delay`, as the connectors server does while it reads
    /// each connected account's tools from its service. Its address.
    async fn slow_mcp_server(delay: std::time::Duration) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let end = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                    let length: usize = head
                        .lines()
                        .filter_map(|l| l.split_once(':'))
                        .find(|(k, _)| k.trim() == "content-length")
                        .and_then(|(_, v)| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < end + length {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let request: serde_json::Value =
                        serde_json::from_slice(&buf[end..]).unwrap_or_default();
                    let id = request.get("id").cloned();
                    let result = match request["method"].as_str() {
                        Some("initialize") => Some(serde_json::json!({
                            "protocolVersion": request["params"]["protocolVersion"],
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "slow", "version": "1" },
                        })),
                        Some("tools/list") => {
                            tokio::time::sleep(delay).await;
                            Some(serde_json::json!({ "tools": [{
                                "name": "ping",
                                "description": "Answers pong.",
                                "inputSchema": { "type": "object" },
                            }] }))
                        }
                        _ => None,
                    };
                    // A request it does not serve (a newer protocol's
                    // discovery, say) is answered as unknown, as a server of
                    // the 2025-11-25 protocol does.
                    let answer = |id, outcome: (&str, serde_json::Value)| {
                        let mut body = serde_json::json!({ "jsonrpc": "2.0", "id": id });
                        body[outcome.0] = outcome.1;
                        body.to_string()
                    };
                    let reply = match (head.starts_with("post "), id, result) {
                        (true, Some(id), result) => {
                            let body = match result {
                                Some(result) => answer(id, ("result", result)),
                                None => answer(id, ("error", serde_json::json!({ "code": -32601, "message": "method not found" }))),
                            };
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        }
                        (true, _, _) => "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
                        _ => "HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
                    };
                    let _ = socket.write_all(reply.as_bytes()).await;
                });
            }
        });
        url
    }

    /// A hosted session's configuration, without the confinement, signed in
    /// to a server whose connectors are at `url`: the connectors server is
    /// its one MCP server, and tools are disclosed on demand.
    fn connectors_config(url: &str) -> Config {
        let mut config = mock_config();
        config.tools.disclosure.mode = sven_bootstrap::Disclosure::Deferred;
        crate::restrict_to_hosted(&mut config);
        let login = RemoteLogin {
            server: "http://127.0.0.1".into(),
            control_url: "http://127.0.0.1/api/workspace".into(),
            openai_base_url: "http://127.0.0.1/v1".into(),
            mcp_url: Some(url.into()),
            mcp_token_url: None,
            transfer_origins: Vec::new(),
            token: "host_key".into(),
            workspace_id: Uuid::nil(),
            workspace_name: "Hosted".into(),
            account: "a@example.com".into(),
        };
        use_server_services(&mut config, &login);
        config
    }

    fn mock_model() -> ModelConfig {
        ModelConfig {
            provider: "mock".into(),
            name: "mock-model".into(),
            ..ModelConfig::default()
        }
    }

    const PING: &str = "connectors-ping";

    /// The person asks right away, while the connectors server still reads
    /// their accounts' tools: the session that answers already has them.
    #[tokio::test]
    async fn a_session_opens_with_the_tools_of_a_server_still_listing_them() {
        let dir = tempfile::tempdir().unwrap();
        let url = slow_mcp_server(std::time::Duration::from_secs(2)).await;
        let mut actor = idle_session(dir.path(), connectors_config(&url), 15_000);
        let controller = actor.open_controller(mock_model()).await.expect("opens");
        let registry = controller.tool_registry();
        assert!(
            registry.get(PING).is_some(),
            "the session opened without the server's tools: {:?}",
            registry.names()
        );
        assert!(registry.get(sven_bootstrap::DISCOVERY_TOOL).is_some());
    }

    /// A server slower than the session's wait still reaches it, once it has
    /// listed its tools.
    #[tokio::test]
    async fn a_server_that_lists_its_tools_after_the_session_opened_reaches_it() {
        let dir = tempfile::tempdir().unwrap();
        let url = slow_mcp_server(std::time::Duration::from_secs(1)).await;
        let mut actor = idle_session(dir.path(), connectors_config(&url), 0);
        let controller = actor.open_controller(mock_model()).await.expect("opens");
        let registry = controller.tool_registry();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while registry.get(PING).is_none() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            registry.get(PING).is_some(),
            "the server's tools never reached the session: {:?}",
            registry.names()
        );
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

    /// Spec: the server's `notify` is told to the conversation as a notice, not
    /// as the person's message. It starts no turn unless asked; one that
    /// arrives while a turn runs is held until the turn ends; and the kept
    /// conversation holds it as the host's.
    #[tokio::test]
    async fn a_notice_is_told_as_a_notice_and_starts_a_turn_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let mut actor = running_session(dir.path()).await;
        let notice = sven_vocab::Notice::new("job.finished", "The render is ready.").unwrap();

        actor.notice(notice.clone(), false).await;
        assert!(!actor.working, "no turn was asked for");
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), actor.events_rx.recv())
                .await
                .expect("the notice is announced")
                .expect("the session is alive");
            let announced = matches!(&event, AgentEvent::Notice(n) if *n == notice);
            assert!(
                !matches!(event, AgentEvent::TurnComplete),
                "no turn was asked for"
            );
            actor.forward(event).await;
            if announced {
                break;
            }
        }
        let kept = actor.context.sessions.load(actor.id).expect("kept copy");
        let last = kept.last().unwrap();
        assert_eq!(last.role, sven_model::Role::User);
        assert_eq!(
            sven_vocab::Notice::parse(last.as_text().unwrap()),
            Some(notice.clone())
        );

        // Asked for a turn, the session answers it, and a notice told while
        // that turn runs waits for the turn's end.
        actor.notice(notice.clone(), true).await;
        assert!(actor.working);
        actor.notice(notice.clone(), false).await;
        assert_eq!(actor.held_notices.len(), 1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let event = tokio::time::timeout_at(deadline, actor.events_rx.recv())
                .await
                .expect("the turn ends in time")
                .expect("the session is alive");
            let over = matches!(event, AgentEvent::TurnComplete);
            actor.forward(event).await;
            if over {
                break;
            }
        }
        assert!(!actor.working);
        assert!(actor.held_notices.is_empty(), "the held notice was told");
    }

    /// A goal is one answer to the server: its turns follow each other with
    /// no end in between, the answer ends when the goal stops, and the goal
    /// is kept, so a host started again on the same directory has it back.
    #[tokio::test]
    async fn a_goal_runs_as_one_answer_and_a_restarted_host_has_it_back() {
        use sven_vocab::goal::GoalStatus;
        let dir = tempfile::tempdir().unwrap();
        let mut actor = running_session(dir.path()).await;
        actor.answer("/goal tidy the readme".into(), &[]).await;
        assert!(actor.working, "the answer is running");

        let mut ended = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while ended == 0 {
            let event = tokio::time::timeout_at(deadline, actor.events_rx.recv())
                .await
                .expect("the goal ends in time")
                .expect("the session is alive");
            if matches!(event, AgentEvent::TurnComplete) {
                ended += 1;
            }
            actor.forward(event).await;
        }
        assert!(!actor.working, "the answer ended with the goal");
        // The mock model answers without ever acting: the goal took its turns
        // in that one answer and was blocked by the supervisor.
        let kept = actor
            .context
            .sessions
            .load_goal(actor.id)
            .expect("the goal is kept");
        assert_eq!(kept.status, GoalStatus::Blocked);
        assert_eq!(kept.turns_used, 3);

        let mut again = SessionActor::new(actor.id, Arc::clone(&actor.context), Arc::default());
        again
            .handle(SessionCommand::Open {
                mode: AgentMode::Agent,
                history: Vec::new(),
                replace: false,
            })
            .await;
        assert_eq!(again.goal.map(|g| g.status), Some(GoalStatus::Blocked));
    }

    /// The server asks how a goal stands; a session this run has not opened
    /// answers from what it kept, without opening one.
    #[tokio::test]
    async fn a_goal_is_reported_from_what_was_kept_without_opening_the_session() {
        use sven_vocab::goal::GoalStatus;
        let dir = tempfile::tempdir().unwrap();
        let (uploads, mut sent) = mpsc::channel(64);
        let (asks, _asks_open) = mpsc::channel(8);
        let context = Arc::new(SessionContext {
            config: Arc::new(mock_config()),
            working_dir: dir.path().to_path_buf(),
            hosted: false,
            wide_access: true,
            shared_workdir: false,
            openai_base_url: String::new(),
            token: String::new(),
            client: crate::client::RemoteClient::new("http://127.0.0.1:9/api", "token").unwrap(),
            uploads,
            sessions: SessionStore::new(dir.path().join("kept")),
            asks,
            mcp_tools_wait_ms: 0,
        });
        let id = Uuid::new_v4();
        let mut kept = Goal::new("goal-1".into(), "port the parser".into(), 20, None);
        kept.status = GoalStatus::Paused;
        context.sessions.save_goal(id, Some(&kept)).unwrap();

        let mut actor = SessionActor::new(id, Arc::clone(&context), Arc::default());
        actor.handle(SessionCommand::ReportGoal).await;
        assert!(actor.controller.is_none(), "nothing was opened to answer");
        let Some(Upload::Event(event)) = sent.recv().await else {
            panic!("an answer is uploaded");
        };
        match *event {
            ControlEvent::Goal { session_id, goal } => {
                assert_eq!(session_id, id);
                assert_eq!(goal.map(|g| g.status), Some(GoalStatus::Paused));
            }
            other => panic!("{other:?}"),
        }
    }

    /// The person stops a goal and at once sends the next request. The
    /// server already ended the stopped answer and shows the new one; every
    /// event the stopped turn still sends names the stopped answer, so it
    /// never ends or fills the new one, and the new request waits for the
    /// stopped turn instead of being lost to it.
    #[tokio::test]
    async fn a_stopped_answers_late_events_never_reach_the_next_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (uploads, mut sent) = mpsc::channel(UPLOAD_QUEUE);
        let (asks, _asks_open) = mpsc::channel(8);
        let context = Arc::new(SessionContext {
            config: Arc::new(mock_config()),
            working_dir: dir.path().to_path_buf(),
            hosted: false,
            wide_access: true,
            shared_workdir: false,
            openai_base_url: String::new(),
            token: String::new(),
            client: crate::client::RemoteClient::new("http://127.0.0.1:9/api", "token").unwrap(),
            uploads,
            sessions: SessionStore::new(dir.path().join("kept")),
            asks,
            mcp_tools_wait_ms: 0,
        });
        let mut actor = SessionActor::new(Uuid::new_v4(), context, Arc::default());
        let model = ModelConfig {
            provider: "mock".into(),
            name: "mock-model".into(),
            ..ModelConfig::default()
        };
        actor.controller = Some(actor.open_controller(model).await.expect("opens"));
        let (commands, inbox) = mpsc::channel(SESSION_QUEUE);
        let task = tokio::spawn(actor.run(inbox));
        let input = |text: &str, id: &str| {
            SessionCommand::Input(answers::Input {
                text: text.into(),
                answer_id: Some(id.into()),
                attachments: Vec::new(),
                steer: false,
                ask_every_tool: false,
            })
        };
        commands
            .send(input("/goal tidy the readme", "a"))
            .await
            .unwrap();
        commands.send(SessionCommand::Cancel).await.unwrap();
        commands.send(input("/goal resume", "b")).await.unwrap();

        let mut tagged = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let upload = tokio::time::timeout_at(deadline, sent.recv())
                .await
                .expect("the next answer ends in time")
                .expect("the session is alive");
            let Upload::Event(event) = upload else {
                continue;
            };
            let ControlEvent::Session {
                event, answer_id, ..
            } = *event
            else {
                continue;
            };
            let ending = matches!(event, AgentEvent::TurnComplete | AgentEvent::Aborted { .. });
            tagged.push((answer_id.clone(), ending));
            if ending && answer_id.as_deref() == Some("b") {
                break;
            }
        }
        task.abort();

        assert!(
            tagged.iter().all(|(id, _)| id.is_some()),
            "every event names its answer: {tagged:?}"
        );
        let first_of_b = tagged
            .iter()
            .position(|(id, _)| id.as_deref() == Some("b"))
            .unwrap();
        let (before, after) = tagged.split_at(first_of_b);
        assert!(
            before
                .iter()
                .any(|(id, ending)| *ending && id.as_deref() == Some("a")),
            "the stopped answer ended before the next began: {tagged:?}"
        );
        assert!(
            after.iter().all(|(id, _)| id.as_deref() == Some("b")),
            "nothing of the stopped answer lands in the next: {tagged:?}"
        );
        assert_eq!(
            after.iter().filter(|(_, ending)| *ending).count(),
            1,
            "the next answer ends once: {tagged:?}"
        );
    }

    /// A session on the scripted model of `responses`, opened, with what it
    /// uploads and the permission it asks for.
    async fn scripted_session(
        dir: &std::path::Path,
        responses: &str,
    ) -> (
        SessionActor,
        mpsc::Receiver<Upload>,
        mpsc::Receiver<ControlEvent>,
    ) {
        let script = dir.join("responses.yaml");
        std::fs::write(&script, responses).unwrap();
        let (uploads, sent) = mpsc::channel(UPLOAD_QUEUE);
        let (asks, asked) = mpsc::channel(8);
        let context = Arc::new(SessionContext {
            config: Arc::new(mock_config()),
            working_dir: dir.to_path_buf(),
            hosted: false,
            wide_access: true,
            shared_workdir: false,
            openai_base_url: String::new(),
            token: String::new(),
            client: crate::client::RemoteClient::new("http://127.0.0.1:9/api", "token").unwrap(),
            uploads,
            sessions: SessionStore::new(dir.join("kept")),
            asks,
            mcp_tools_wait_ms: 0,
        });
        let mut actor = SessionActor::new(Uuid::new_v4(), context, Arc::default());
        let model = ModelConfig {
            provider: "mock".into(),
            name: "mock-model".into(),
            mock_responses_file: Some(script.to_string_lossy().into_owned()),
            ..ModelConfig::default()
        };
        actor.model = Some(model.name.clone());
        actor.scripted_model = Some(model.clone());
        actor.controller = Some(actor.open_controller(model).await.expect("opens"));
        (actor, sent, asked)
    }

    fn typed(text: &str, id: &str, steer: bool) -> SessionCommand {
        SessionCommand::Input(answers::Input {
            text: text.into(),
            answer_id: Some(id.into()),
            attachments: Vec::new(),
            steer,
            ask_every_tool: false,
        })
    }

    /// The session's events as (answer, event), until `last` ends; each
    /// permission asked for is given.
    async fn events_until_ended(
        sent: &mut mpsc::Receiver<Upload>,
        asked: &mut mpsc::Receiver<ControlEvent>,
        commands: &mpsc::Sender<SessionCommand>,
        mut on_event: impl FnMut(&AgentEvent) -> Option<SessionCommand>,
        last: &str,
    ) -> Vec<(Option<String>, AgentEvent)> {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let upload = tokio::select! {
                upload = sent.recv() => upload.expect("the session is alive"),
                Some(ask) = asked.recv() => {
                    if let ControlEvent::ToolNeedsApproval { call_id, .. } = ask {
                        commands.send(SessionCommand::Decide { call_id, answer: Permission::Allowed }).await.unwrap();
                    }
                    continue;
                }
                () = tokio::time::sleep_until(deadline) => panic!("{last} did not end in time: {seen:?}"),
            };
            let Upload::Event(event) = upload else {
                continue;
            };
            let ControlEvent::Session {
                event, answer_id, ..
            } = *event
            else {
                continue;
            };
            if let Some(command) = on_event(&event) {
                commands.send(command).await.unwrap();
            }
            let ending = matches!(event, AgentEvent::TurnComplete | AgentEvent::Aborted { .. });
            let done = ending && answer_id.as_deref() == Some(last);
            seen.push((answer_id, event));
            if done {
                return seen;
            }
        }
    }

    fn endings(seen: &[(Option<String>, AgentEvent)]) -> Vec<&str> {
        seen.iter()
            .filter(|(_, event)| matches!(event, AgentEvent::TurnComplete))
            .map(|(id, _)| id.as_deref().unwrap_or("-"))
            .collect()
    }

    fn text_of(seen: &[(Option<String>, AgentEvent)], id: &str) -> String {
        seen.iter()
            .filter(|(answer, _)| answer.as_deref() == Some(id))
            .filter_map(|(_, event)| match event {
                AgentEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn typed_by_another(text: &str, id: &str, steer: bool) -> SessionCommand {
        SessionCommand::Input(answers::Input {
            text: text.into(),
            answer_id: Some(id.into()),
            attachments: Vec::new(),
            steer,
            ask_every_tool: true,
        })
    }

    /// Spec: a turn the person stops while a tool of it is still running
    /// ends at once, with `Aborted`: the tool is stopped with it rather than
    /// waited for, so the server, which stops a host whose stopped turn does
    /// not end within its grace (and every answer that host is writing with
    /// it), never has to.
    #[tokio::test]
    async fn a_turn_stopped_while_its_tool_runs_ends_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let (actor, mut sent, mut asked) = scripted_session(
            dir.path(),
            r#"
responses:
  - match_type: contains
    pattern: "wait for it"
    tool_calls:
      - id: hangs
        tool: shell
        args:
          shell_command: "sleep 600"
    after_tool_reply: "never"
"#,
        )
        .await;
        let (commands, inbox) = mpsc::channel(SESSION_QUEUE);
        let task = tokio::spawn(actor.run(inbox));
        commands
            .send(typed("wait for it", "a", false))
            .await
            .unwrap();
        let started = tokio::time::Instant::now();
        let mut stopped_at = None;
        let seen = tokio::time::timeout(
            Duration::from_secs(20),
            events_until_ended(
                &mut sent,
                &mut asked,
                &commands,
                |event| match event {
                    AgentEvent::ToolCallStarted(_) if stopped_at.is_none() => {
                        stopped_at = Some(tokio::time::Instant::now());
                        Some(SessionCommand::Cancel)
                    }
                    _ => None,
                },
                "a",
            ),
        )
        .await
        .expect("the stopped turn ends");
        task.abort();
        let stopped_at = stopped_at.expect("the tool started");
        assert!(
            matches!(seen.last(), Some((_, AgentEvent::Aborted { .. }))),
            "the turn ended stopped: {seen:?}"
        );
        assert!(
            stopped_at.elapsed() < Duration::from_secs(5),
            "it ended within 5 s of the stop, {:?} after it began",
            started.elapsed()
        );
    }

    /// Spec (the server's model of shared turns): a message someone the
    /// conversation is shared with sent while the owner's turn runs never
    /// steers it: it is answered as a turn of its own, after it, and that
    /// turn puts even a file read to the server, naming its own answer; the
    /// owner's turn reads unasked as before.
    #[tokio::test]
    async fn another_persons_message_never_rides_on_the_owners_turn() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "owner's notes").unwrap();
        let (actor, mut sent, mut asked) = scripted_session(
            dir.path(),
            r#"
responses:
  - match_type: contains
    pattern: "show me the notes"
    tool_calls:
      - id: theirs
        tool: read_file
        args:
          path: "notes.txt"
    after_tool_reply: "their answer"
  - match_type: contains
    pattern: "tidy the notes"
    tool_calls:
      - id: mine
        tool: read_file
        args:
          path: "notes.txt"
      - id: slow
        tool: shell
        args:
          shell_command: "sleep 2"
    after_tool_reply: "owner answer"
"#,
        )
        .await;
        let (commands, inbox) = mpsc::channel(SESSION_QUEUE);
        let task = tokio::spawn(actor.run(inbox));
        commands
            .send(typed("tidy the notes", "a", false))
            .await
            .unwrap();
        let mut sent_theirs = false;
        let mut asks: Vec<(String, String, Option<String>)> = Vec::new();
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let upload = tokio::select! {
                upload = sent.recv() => upload.expect("the session is alive"),
                Some(ask) = asked.recv() => {
                    if let ControlEvent::ToolNeedsApproval { call_id, tool_name, answer_id, .. } = ask {
                        asks.push((call_id.clone(), tool_name, answer_id));
                        commands.send(SessionCommand::Decide { call_id, answer: Permission::Allowed }).await.unwrap();
                    }
                    continue;
                }
                () = tokio::time::sleep_until(deadline) => panic!("b did not end in time: {seen:?}"),
            };
            let Upload::Event(event) = upload else {
                continue;
            };
            let ControlEvent::Session {
                event, answer_id, ..
            } = *event
            else {
                continue;
            };
            if matches!(event, AgentEvent::ToolCallStarted(_))
                && !std::mem::replace(&mut sent_theirs, true)
            {
                // Sent to steer, by someone else, while the owner's tool runs.
                commands
                    .send(typed_by_another("show me the notes", "b", true))
                    .await
                    .unwrap();
            }
            let done =
                matches!(event, AgentEvent::TurnComplete) && answer_id.as_deref() == Some("b");
            seen.push((answer_id, event));
            if done {
                break;
            }
        }
        task.abort();
        assert_eq!(
            endings(&seen),
            ["a", "b"],
            "answered after the owner's turn, as its own: {seen:?}"
        );
        assert!(text_of(&seen, "a").contains("owner answer"));
        assert!(text_of(&seen, "b").contains("their answer"));
        let reads: Vec<_> = asks
            .iter()
            .filter(|(_, tool, _)| tool == "read_file")
            .collect();
        assert_eq!(
            reads
                .iter()
                .map(|(call, _, answer)| (call.as_str(), answer.as_deref()))
                .collect::<Vec<_>>(),
            [("theirs", Some("b"))],
            "only the other person's read asks, naming their answer: {asks:?}"
        );
    }

    /// The person sends a message while a tool runs, to steer the turn. The
    /// turn is not stopped: it takes the message after the tool's result,
    /// the answer it was writing ends there, and what it writes next is the
    /// steering message's answer, which ends once with the turn.
    #[tokio::test]
    async fn a_steering_message_is_answered_by_the_running_turn() {
        let dir = tempfile::tempdir().unwrap();
        let (actor, mut sent, mut asked) = scripted_session(
            dir.path(),
            r#"
responses:
  - match_type: contains
    pattern: "use the new lexer"
    reply: "lexer answer"
  - match_type: contains
    pattern: "fix the parser"
    tool_calls:
      - id: slow
        tool: shell
        args:
          shell_command: "sleep 2"
    after_tool_reply: "parser answer"
"#,
        )
        .await;
        let (commands, inbox) = mpsc::channel(SESSION_QUEUE);
        let task = tokio::spawn(actor.run(inbox));
        commands
            .send(typed("fix the parser", "a", false))
            .await
            .unwrap();
        let mut steered = false;
        let seen = events_until_ended(
            &mut sent,
            &mut asked,
            &commands,
            |event| match event {
                AgentEvent::ToolCallStarted(_) if !std::mem::replace(&mut steered, true) => {
                    Some(typed("use the new lexer", "b", true))
                }
                _ => None,
            },
            "b",
        )
        .await;
        task.abort();

        assert_eq!(
            endings(&seen),
            ["a", "b"],
            "each answer ends once, in order: {seen:?}"
        );
        let at = seen
            .iter()
            .position(|(_, event)| matches!(event, AgentEvent::Steered { .. }))
            .expect("the turn says it took the message");
        assert_eq!(seen[at].0.as_deref(), Some("b"));
        assert!(
            seen[..at].iter().all(|(id, _)| id.as_deref() == Some("a")),
            "everything before it is the first answer's: {seen:?}"
        );
        assert_eq!(text_of(&seen, "b"), "lexer answer");
        assert!(
            !seen.iter().any(
                |(_, e)| matches!(e, AgentEvent::TextDelta(t) if t.contains("A turn is running"))
            ),
            "never refused: {seen:?}"
        );
    }

    /// A message sent while a turn still runs (one stopped, winding a long
    /// tool down; or one that never stopped) is queued, not refused: it is
    /// answered as soon as that turn ends, and its answer ends once.
    #[tokio::test]
    async fn a_message_sent_while_a_turn_runs_is_answered_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut actor, mut sent, mut asked) = scripted_session(
            dir.path(),
            r#"
responses:
  - match_type: contains
    pattern: "do not use the toolkit"
    reply: "running it as asked"
"#,
        )
        .await;
        actor.asking = Some("a".into());
        actor.expect_ending();
        actor.asking = None;
        actor.state(SessionState::Running).await;
        let events = actor.events_tx.clone();
        let (commands, inbox) = mpsc::channel(SESSION_QUEUE);
        let task = tokio::spawn(actor.run(inbox));
        commands
            .send(typed("do not use the toolkit", "b", false))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        events.send(AgentEvent::TurnComplete).await.unwrap();
        let seen = events_until_ended(&mut sent, &mut asked, &commands, |_| None, "b").await;
        task.abort();

        assert_eq!(
            endings(&seen),
            ["a", "b"],
            "each answer ends once, in order: {seen:?}"
        );
        assert_eq!(text_of(&seen, "b"), "running it as asked");
        assert!(
            !seen.iter().any(
                |(_, e)| matches!(e, AgentEvent::TextDelta(t) if t.contains("A turn is running"))
            ),
            "never refused: {seen:?}"
        );
    }

    /// A message over the limit every client keeps to never reaches the
    /// model: its answer ends at once, saying its size and to attach it.
    #[tokio::test]
    async fn a_message_over_the_limit_is_refused_saying_to_attach_it() {
        let dir = tempfile::tempdir().unwrap();
        let (actor, mut sent, mut asked) = scripted_session(
            dir.path(),
            r#"
responses:
  - match_type: contains
    pattern: "a"
    reply: "the model read it"
"#,
        )
        .await;
        let (commands, inbox) = mpsc::channel(SESSION_QUEUE);
        let task = tokio::spawn(actor.run(inbox));
        let long = "a".repeat(sven_vocab::message::MAX_MESSAGE_BYTES + 1);
        commands.send(typed(&long, "b", false)).await.unwrap();
        let seen = events_until_ended(&mut sent, &mut asked, &commands, |_| None, "b").await;
        task.abort();

        assert_eq!(endings(&seen), ["b"], "{seen:?}");
        assert!(
            seen.iter().any(|(_, e)| matches!(e, AgentEvent::Error(m)
                if m == &sven_vocab::message::too_long(long.len()))),
            "refused, naming the size and the way out: {seen:?}"
        );
        assert!(
            !text_of(&seen, "b").contains("the model read it"),
            "{seen:?}"
        );
    }

    /// A steering message the turn ends without taking (it was past its last
    /// step) is not lost: it is answered next, as a message of its own.
    #[tokio::test]
    async fn a_steering_message_the_turn_missed_is_answered_next() {
        let dir = tempfile::tempdir().unwrap();
        let (mut actor, mut sent, mut asked) = scripted_session(
            dir.path(),
            r#"
responses:
  - match_type: contains
    pattern: "use the new lexer"
    reply: "lexer answer"
"#,
        )
        .await;
        // A turn the host is still writing answer "a" of, whose session has
        // already come to rest: the steering message reaches nothing.
        actor.asking = Some("a".into());
        actor.expect_ending();
        actor.asking = None;
        actor.state(SessionState::Running).await;
        let events = actor.events_tx.clone();
        let (commands, inbox) = mpsc::channel(SESSION_QUEUE);
        let task = tokio::spawn(actor.run(inbox));
        commands
            .send(typed("use the new lexer", "b", true))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        events.send(AgentEvent::TurnComplete).await.unwrap();
        let seen = events_until_ended(&mut sent, &mut asked, &commands, |_| None, "b").await;
        task.abort();

        assert_eq!(endings(&seen), ["a", "b"], "{seen:?}");
        assert_eq!(text_of(&seen, "b"), "lexer answer");
    }

    #[test]
    fn a_long_prompt_prints_as_its_first_line_cut_at_a_character() {
        assert_eq!(first_line("list files\nthen more"), "list files");
        let long = "é".repeat(150);
        assert_eq!(first_line(&long).chars().count(), 100);
    }
}
