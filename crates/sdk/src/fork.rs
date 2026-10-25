// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Starting an agent from a recorded transcript and a workspace snapshot.
//!
//! A session normally starts clean. [`Engine::fork`] starts one from work that
//! already happened: the transcript a model was shown up to a step, and the
//! workspace the agent had left at that step, recorded as
//! [`RecordedState`]. Every tool call the transcript holds is replayed, in
//! order, through the tools this engine offers, into a fresh workspace; the
//! fork is admitted only when
//!
//! * every replayed observation is the one the transcript recorded
//!   ([`ForkError::ResultMismatch`]),
//! * the workspace it leaves hashes to the digest recorded at that step
//!   ([`ForkError::WorkspaceMismatch`]), and
//! * the visible transcript hashes to the one recorded
//!   ([`ForkError::TranscriptMismatch`]).
//!
//! A refused fork replays nothing further and returns no agent: the caller
//! learns which of the three disagreed, and why. An admitted fork is an
//! ordinary [`Agent`] whose conversation is the recorded one,
//! ready to continue on the model the engine was built with - which need not
//! be the model that produced the transcript. [`Agent::continue_with`]
//! continues it without inventing a user turn.
//!
//! What a recording must make reproducible: the recorded observations are
//! what the model was shown, so a replayed result is compared after the same
//! deterministic truncation, error marking and repeat note the tool executor
//! applies. A call whose effect is not the workspace is refused rather than
//! approximated ([`ForkError::Unreplayable`]): delegation, asking a person,
//! memory, skills and the web all read state that outlives the tree.
//!
//! Swedish Embedded AB implements agent runtimes that can continue a recorded
//! session on another model, for its clients. If your team needs expertise in
//! on-policy agent data then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};
use sven_bootstrap::{build_tool_registry, ToolSetProfile};
use sven_model::{Message, MessageContent, Role};
use sven_tool_api::{PathScope, ToolCall};
use sven_tool_registry::ToolRegistry;
use sven_tools_fs::OutputBufferStore;
use sven_turn::AgentRuntimeContext;
use sven_vocab::AgentMode;
use walkdir::WalkDir;

use crate::agent::Agent;
use crate::engine::Engine;
use crate::error::CallError;
use crate::state::AgentState;
use crate::transcript::{ToolCallRecord, Turn};

/// The toolset sentinel of a session built with no tools at all.
const MODE_AGENT: &str = "agent";

/// Tools whose effect is not the workspace and cannot be replayed from a
/// transcript: each either starts another model run, asks a person, changes
/// the session itself, or reads state that outlives it.
const UNREPLAYABLE: &[(&str, &str)] = &[
    ("task", "starts another agent's run"),
    ("ask_question", "waits for a person"),
    ("system", "changes the session's mode or model"),
    ("memory", "reads state that outlives the workspace"),
    ("semantic_memory", "reads state that outlives the workspace"),
    ("skill", "brings in instructions from outside the workspace"),
    (
        "web_fetch",
        "reads the network, which the workspace does not record",
    ),
    (
        "web_search",
        "reads the network, which the workspace does not record",
    ),
];

/// The digests a recording holds: the workspace at the fork step, and the
/// visible transcript up to it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordedState {
    /// [`workspace_digest`] of the tree the agent had left at the step.
    pub workspace: String,
    /// [`transcript_digest`] of the turns the model was shown.
    pub transcript: String,
}

impl RecordedState {
    /// `workspace` and `transcript` as recorded.
    #[must_use]
    pub fn new(workspace: impl Into<String>, transcript: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            transcript: transcript.into(),
        }
    }

    /// The recording of `transcript` against the workspace `root` holds now.
    ///
    /// # Errors
    ///
    /// As [`workspace_digest`].
    pub fn of(transcript: &[Turn], root: &Path) -> io::Result<Self> {
        Ok(Self {
            workspace: workspace_digest(root)?,
            transcript: transcript_digest(transcript),
        })
    }
}

/// One recorded action: the tool the model asked for, the arguments it sent,
/// and the observation the transcript answered it with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedCall {
    /// The model's call id, which pairs the action with its observation.
    pub call_id: String,
    /// The tool's name.
    pub tool: String,
    /// The arguments exactly as the model emitted them.
    pub arguments: String,
    /// The observation as the model was shown it.
    pub result: String,
}

/// A fork to verify and start: the recorded transcript, the fresh workspace
/// to replay it into, and the recording both are held to.
#[derive(Clone, Debug)]
pub struct Fork {
    transcript: Vec<Turn>,
    workspace: PathBuf,
    recorded: RecordedState,
    mode: String,
}

/// The observation `text` with the values the *sandbox* injects removed, so a
/// replay in a fresh workspace can be compared to a recording made in another
/// one: the per-attempt temporary directory a workspace, a `.sven` state
/// symlink or a traceback names (every attempt has its own, so the path always
/// differs), the clock's timestamp column in an `ls -l`-style listing (a replay
/// runs later, so every directory and the symlink show a new time), and the
/// numbers on the sandbox's own entries - `.` and `..` describe the temporary
/// directory the workspace was copied into, and `.sven`'s size is the length of
/// its target's path, so both move between attempts. The advisory note the
/// harness appends to a repeated result is not the tool's output either.
///
/// Nothing the student caused is removed: file names, sizes, link targets
/// outside the temporary directory, command output and exit text stay, and the
/// student's tree is checked by its digest (`RecordedState::workspace`) rather
/// than by this text.
fn without_harness_values(text: &str) -> std::borrow::Cow<'_, str> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    if !text.contains(".tmp")
        && !MONTHS.iter().any(|m| text.contains(m))
        && !text.contains(HARNESS_NOTE)
    {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut lines: Vec<String> = Vec::with_capacity(text.lines().count());
    for line in text.split('\n') {
        if line.trim_start().starts_with(HARNESS_NOTE) {
            continue;
        }
        lines.push(without_listing_values(line, &MONTHS));
    }
    // A note that closed the observation leaves its blank line behind.
    let mut out = lines.join("\n");
    out.truncate(out.trim_end().len());
    std::borrow::Cow::Owned(out)
}

/// The first words of the note the harness appends to a result a previous call
/// returned unchanged.
const HARNESS_NOTE: &str = "[sven note:";

/// One line of `without_harness_values`: the sandbox's numbers removed from an
/// `ls -l`-style listing, whitespace collapsed, and every path into a temporary
/// directory reduced to `<tmp>`.
fn without_listing_values(line: &str, months: &[&str; 12]) -> String {
    // Whitespace runs are collapsed, so a listing's aligned columns compare
    // equal without depending on the width of a number.
    let fields: Vec<&str> = line.split_whitespace().collect();
    // A listing's block total counts what the copy allocated, not the student's
    // work, so two listings of the same tree can total differently.
    if matches!(fields.as_slice(), ["total", digits] if digits.chars().all(|c| c.is_ascii_digit()))
    {
        return "total".to_string();
    }
    let is_listing = fields.first().is_some_and(|f| {
        f.len() >= 10 && f.chars().next().is_some_and(|c| "[-dlbcps]".contains(c))
    });
    // `ls -l` puts the name after mode, links, owner, group, size and the three
    // fields of a timestamp; a listing whose month is there is one.
    let stamp = is_listing
        && fields.len() >= 9
        && months.contains(&fields[5])
        && (fields[7].contains(':') || fields[7].len() == 4);
    let sandbox = stamp && matches!(fields.get(8), Some(&".") | Some(&"..") | Some(&".sven"));
    let mut kept: Vec<&str> = Vec::with_capacity(fields.len());
    for (j, f) in fields.iter().enumerate() {
        if sandbox {
            // The sandbox entry itself, by its mode and its name: everything
            // between and after describes the temporary directory.
            if j == 0 || j == 8 {
                kept.push(f);
            }
            continue;
        }
        if stamp && (5..8).contains(&j) {
            if j == 5 {
                kept.push("<time>");
            }
            continue;
        }
        kept.push(f);
    }
    let mut out = String::with_capacity(line.len());
    for (k, f) in kept.iter().enumerate() {
        if k > 0 {
            out.push(' ');
        }
        out.push_str(&without_temp_path(f));
    }
    out
}

/// `token` with a path into a temporary directory reduced to `<tmp>`, so what
/// it names beside that path stays comparable: `"…/.tmpVAHVM8/tree/x.py",`
/// becomes `"<tmp>/tree/x.py",` and `/…/.tmpXY` becomes `<tmp>`.
fn without_temp_path(token: &str) -> std::borrow::Cow<'_, str> {
    // Where the `.tmpXXXX` path component starts, if the token has one.
    let at = match token.rfind("/.tmp") {
        Some(slash) => slash + 1,
        None if token.starts_with(".tmp") => 0,
        None => return std::borrow::Cow::Borrowed(token),
    };
    let bytes = token.as_bytes();
    let mut end = at + 4;
    while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
        end += 1;
    }
    // Where the path itself starts: after the quote, parenthesis or assignment
    // that introduced it, else at the token's start.
    let start = token[..at]
        .rfind(['"', '\'', '(', '=', ':', ','])
        .map_or(0, |i| i + 1);
    let mut out = String::with_capacity(token.len() - (end - start) + 5);
    out.push_str(&token[..start]);
    out.push_str("<tmp>");
    out.push_str(&token[end..]);
    std::borrow::Cow::Owned(out)
}

impl Fork {
    /// A fork of `transcript` into `workspace`, held to `recorded`.
    #[must_use]
    pub fn new(
        transcript: Vec<Turn>,
        workspace: impl Into<PathBuf>,
        recorded: RecordedState,
    ) -> Self {
        Self {
            transcript,
            workspace: workspace.into(),
            recorded,
            mode: MODE_AGENT.to_string(),
        }
    }

    /// The same fork running `mode` instead of the conversational agent.
    #[must_use]
    pub fn in_mode(mut self, mode: impl Into<String>) -> Self {
        self.mode = mode.into();
        self
    }

    /// The transcript the fork replays and continues.
    #[must_use]
    pub fn transcript(&self) -> &[Turn] {
        &self.transcript
    }

    /// The fresh workspace the replay writes into.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The recording the fork is held to.
    #[must_use]
    pub fn recorded(&self) -> &RecordedState {
        &self.recorded
    }

    /// The recorded actions, in the order the model took them, each paired
    /// with the observation that answered it.
    ///
    /// # Errors
    ///
    /// [`ForkError::UnansweredCall`] for a call with no observation, and
    /// [`ForkError::ResultWithoutCall`] for an observation of no call: either
    /// means the transcript is not a complete prefix of a session.
    /// [`ForkError::Unrepresentable`] for a turn this build cannot replay.
    pub fn calls(&self) -> Result<Vec<RecordedCall>, ForkError> {
        let mut calls: Vec<RecordedCall> = Vec::new();
        let mut waiting: BTreeMap<&str, usize> = BTreeMap::new();
        for turn in &self.transcript {
            match turn {
                Turn::Assistant { tool_calls, .. } => {
                    for call in tool_calls {
                        waiting.insert(&call.id, calls.len());
                        calls.push(RecordedCall {
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            arguments: call.arguments.clone(),
                            result: String::new(),
                        });
                    }
                }
                Turn::ToolResult { call_id, text } => {
                    let Some(at) = waiting.remove(call_id.as_str()) else {
                        return Err(ForkError::ResultWithoutCall {
                            call_id: call_id.clone(),
                        });
                    };
                    calls[at].result.clone_from(text);
                }
                Turn::Opaque { role } => {
                    return Err(ForkError::Unrepresentable {
                        reason: format!("a {role} turn holds content a transcript cannot replay"),
                    })
                }
                Turn::System { .. } | Turn::User { .. } => {}
            }
        }
        if let Some((call_id, _)) = waiting.into_iter().next() {
            return Err(ForkError::UnansweredCall {
                call_id: call_id.to_string(),
            });
        }
        Ok(calls)
    }
}

/// How many conversation messages `transcript` becomes, excluding the system
/// prompt it records: the length of the history a fork of it starts with.
///
/// A turn holding text and several calls is several messages, exactly as the
/// kernel stores one model turn. A session that continues a fork exports one
/// trajectory step per message, so this is the number of its leading steps
/// that belong to the recorded transcript.
#[must_use]
pub fn message_count(transcript: &[Turn]) -> usize {
    transcript
        .iter()
        .map(|turn| match turn {
            Turn::System { .. } => 0,
            Turn::User { .. } | Turn::ToolResult { .. } | Turn::Opaque { .. } => 1,
            Turn::Assistant { text, tool_calls } => {
                let words = usize::from(!text.is_empty());
                let calls = tool_calls.len();
                if words + calls == 0 {
                    1
                } else {
                    words + calls
                }
            }
        })
        .sum()
}

/// The content address of `transcript`: the SHA-256 of its canonical JSON.
#[must_use]
pub fn transcript_digest(transcript: &[Turn]) -> String {
    let bytes = serde_json::to_vec(transcript).expect("a transcript serializes to JSON");
    format!("sha256:{}", hex(&Sha256::digest(&bytes)))
}

/// The content address of the tree under `root`: every path, its kind and the
/// digest of its contents, sorted by path.
///
/// The exclusions keep the digest the workspace rather than the bookkeeping
/// around it: `.git` (the repository a snapshot is laid out from, whose index
/// and hooks are not the agent's work), `.sven` (the session's own audit log)
/// and `__pycache__` (an interpreter's cached bytecode, which no tool's
/// arguments or output name as the work). They are skipped wherever they
/// appear, so a repository nested in the tree does not enter it either.
/// Symbolic links are digested by their target and never followed, so a
/// digest cannot escape the tree.
///
/// # Errors
///
/// When `root` is not a directory, or an entry cannot be read.
pub fn workspace_digest(root: &Path) -> io::Result<String> {
    if !std::fs::metadata(root)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    let hidden = |path: &Path| {
        let rel = path.strip_prefix(root).unwrap_or(path);
        rel.components().any(|part| {
            matches!(
                part.as_os_str().to_str(),
                Some(".git" | ".sven" | "__pycache__")
            )
        })
    };
    let walk = WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| entry.path() == root || !hidden(entry.path()));
    let mut entries: Vec<(Vec<u8>, u8, Vec<u8>)> = Vec::new();
    for entry in walk {
        let entry = entry.map_err(io::Error::other)?;
        if entry.path() == root {
            continue;
        }
        let rel = entry.path().strip_prefix(root).map_err(io::Error::other)?;
        let mut path: Vec<u8> = Vec::new();
        for part in rel.components() {
            path.extend_from_slice(part.as_os_str().as_encoded_bytes());
            path.push(b'/');
        }
        let kind = entry.file_type();
        let (mark, digest) = if kind.is_dir() {
            (b'd', Vec::new())
        } else if kind.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            (b'l', target.as_os_str().as_encoded_bytes().to_vec())
        } else if kind.is_file() {
            let mut file = std::fs::File::open(entry.path())?;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let read = io::Read::read(&mut file, &mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            (b'f', hasher.finalize().to_vec())
        } else {
            (b'o', Vec::new())
        };
        entries.push((path, mark, digest));
    }
    entries.sort();
    let mut hasher = Sha256::new();
    for (path, mark, digest) in entries {
        hasher.update(&path);
        hasher.update([0, mark, 0]);
        hasher.update(hex(&digest).as_bytes());
        hasher.update(b"\n");
    }
    Ok(format!("sha256:{}", hex(&hasher.finalize())))
}

/// `bytes` as lowercase hexadecimal.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Why a fork was refused.
#[derive(Debug, thiserror::Error)]
pub enum ForkError {
    /// The transcript is not the one recorded at the fork step.
    #[error("the transcript hashes to {actual}, not the recorded {recorded}")]
    TranscriptMismatch {
        /// What the recording holds.
        recorded: String,
        /// What the transcript presented hashes to.
        actual: String,
    },
    /// The workspace the replay left is not the one recorded at the step.
    #[error("the replayed workspace hashes to {actual}, not the recorded {recorded}")]
    WorkspaceMismatch {
        /// What the recording holds.
        recorded: String,
        /// What the replay left.
        actual: String,
    },
    /// A recorded call has no recorded observation.
    #[error("call {call_id} has no recorded observation: the transcript is not a prefix")]
    UnansweredCall {
        /// The call.
        call_id: String,
    },
    /// A recorded observation answers no call.
    #[error("an observation answers {call_id}, which the transcript does not call")]
    ResultWithoutCall {
        /// The observation's call id.
        call_id: String,
    },
    /// A turn this build cannot replay faithfully.
    #[error("the transcript cannot be replayed: {reason}")]
    Unrepresentable {
        /// Why.
        reason: String,
    },
    /// The engine offers no tool of that name.
    #[error("call {call_id} asks for {tool}, which this engine does not offer")]
    UnknownTool {
        /// The call.
        call_id: String,
        /// The tool it names.
        tool: String,
    },
    /// The tool's effect cannot be reproduced from a transcript.
    #[error("call {call_id} uses {tool}, which cannot be replayed: {reason}")]
    Unreplayable {
        /// The call.
        call_id: String,
        /// The tool it names.
        tool: String,
        /// Why not.
        reason: String,
    },
    /// The model's arguments are not JSON, so the call cannot be re-issued.
    #[error("call {call_id}'s arguments to {tool} are not JSON")]
    ArgumentsNotJson {
        /// The call.
        call_id: String,
        /// The tool it names.
        tool: String,
    },
    /// The replayed observation is not the recorded one.
    #[error("call {call_id} to {tool} observed {replayed:?}, not the recorded {recorded:?}")]
    ResultMismatch {
        /// The call.
        call_id: String,
        /// The tool it names.
        tool: String,
        /// What the transcript recorded.
        recorded: String,
        /// What the replay observed.
        replayed: String,
    },
    /// A fork whose engine offers built-in tools needs the engine's model to
    /// build them.
    #[error("a fork with built-in tools needs an engine with a model provider")]
    NoProvider,
    /// The engine does not work in the workspace the fork replays into, so
    /// the agent it would return could not continue there.
    #[error("the engine works in {root:?}, not the fork's workspace {workspace}")]
    EngineRoot {
        /// The fork's workspace.
        workspace: PathBuf,
        /// The engine's project root, when it has one.
        root: Option<PathBuf>,
    },
    /// The workspace or one of its entries could not be read.
    #[error("the workspace {path} could not be read: {source}")]
    Workspace {
        /// The path.
        path: PathBuf,
        /// The failure.
        source: io::Error,
    },
    /// The agent could not be built from the recorded transcript.
    #[error(transparent)]
    Engine(#[from] CallError),
}

impl Engine {
    /// Replays `fork` into its workspace and, when every part of the
    /// recording matches, returns the agent that continues it on this
    /// engine's model.
    ///
    /// The agent's conversation is exactly `fork`'s transcript; its next turn
    /// is asked where the recorded one stopped. The engine must have
    /// [`project_root`](crate::EngineBuilder::project_root) set to the fork's
    /// workspace, because the continuation's tool calls run there.
    ///
    /// # Errors
    ///
    /// [`ForkError`] naming the first part of the recording that did not
    /// match, or the first recorded action that could not be replayed. A
    /// refused fork leaves the workspace as the replay had reached it; it
    /// returns no agent.
    pub async fn fork(&self, fork: &Fork) -> Result<Agent, ForkError> {
        let workspace = &fork.workspace;
        let root = self.project_root().map(Path::to_path_buf);
        let same = match &root {
            Some(root) => match (root.canonicalize(), workspace.canonicalize()) {
                (Ok(root), Ok(workspace)) => root == workspace,
                _ => false,
            },
            None => false,
        };
        if !same {
            return Err(ForkError::EngineRoot {
                workspace: workspace.clone(),
                root,
            });
        }

        let actual = transcript_digest(&fork.transcript);
        if actual != fork.recorded.transcript {
            return Err(ForkError::TranscriptMismatch {
                recorded: fork.recorded.transcript.clone(),
                actual,
            });
        }

        let (role, history) = split_transcript(&fork.transcript)?;
        let calls = fork.calls()?;
        self.replay(fork, &calls).await?;

        let actual = workspace_digest(workspace).map_err(|source| ForkError::Workspace {
            path: workspace.clone(),
            source,
        })?;
        if actual != fork.recorded.workspace {
            return Err(ForkError::WorkspaceMismatch {
                recorded: fork.recorded.workspace.clone(),
                actual,
            });
        }

        let mut state = AgentState::new(fork.mode.clone());
        if let Some(role) = role {
            state = state.with_role(role);
        }
        state.history = history.clone();
        state.record = history;
        self.resume(state).map_err(ForkError::Engine)
    }

    /// Replays every recorded call into the fork's workspace, holding each
    /// observation to the recorded one.
    async fn replay(&self, fork: &Fork, calls: &[RecordedCall]) -> Result<(), ForkError> {
        let registry = self.replay_tools(&fork.workspace)?;
        let config = self.config();
        let cap = config.agent.tool_result_token_cap;
        let repeat_notes = config.agent.note_repeated_results;
        let mut replayed: Vec<Message> = Vec::new();
        let mut results: BTreeMap<&str, &str> = calls
            .iter()
            .map(|call| (call.call_id.as_str(), call.result.as_str()))
            .collect();
        for turn in &fork.transcript {
            match turn {
                Turn::System { .. } => {}
                Turn::User { text } => replayed.push(Message::user(text)),
                Turn::Assistant { text, tool_calls } => {
                    if !text.is_empty() {
                        replayed.push(Message::assistant(text));
                    }
                    for record in tool_calls {
                        let result = results.remove(record.id.as_str()).ok_or_else(|| {
                            ForkError::UnansweredCall {
                                call_id: record.id.clone(),
                            }
                        })?;
                        self.replayed_observation(
                            &registry,
                            record,
                            result,
                            &replayed,
                            cap,
                            repeat_notes,
                        )
                        .await?;
                        replayed.push(tool_call_message(record));
                    }
                }
                Turn::ToolResult { call_id, text } => {
                    replayed.push(Message::tool_result(call_id, text));
                }
                Turn::Opaque { .. } => unreachable!("calls() refused an opaque turn"),
            }
        }
        Ok(())
    }

    /// Runs one recorded call and returns the observation the model would
    /// have been shown, after holding it to the recorded one.
    async fn replayed_observation(
        &self,
        registry: &ToolRegistry,
        record: &ToolCallRecord,
        recorded: &str,
        replayed: &[Message],
        cap: usize,
        repeat_notes: bool,
    ) -> Result<String, ForkError> {
        if let Some((_, reason)) = UNREPLAYABLE.iter().find(|(name, _)| *name == record.name) {
            return Err(ForkError::Unreplayable {
                call_id: record.id.clone(),
                tool: record.name.clone(),
                reason: (*reason).to_string(),
            });
        }
        let Some(tool) = registry.get(&record.name) else {
            return Err(ForkError::UnknownTool {
                call_id: record.id.clone(),
                tool: record.name.clone(),
            });
        };
        let args =
            serde_json::from_str(&record.arguments).map_err(|_| ForkError::ArgumentsNotJson {
                call_id: record.id.clone(),
                tool: record.name.clone(),
            })?;
        let output = tool
            .execute(&ToolCall {
                id: record.id.clone(),
                name: record.name.clone(),
                args,
            })
            .await;
        let category = registry.output_category(&record.name);
        let truncated = sven_turn::smart_truncate(&output.content, category, cap);
        let mut stored = if output.is_error {
            format!("error: {truncated}")
        } else {
            truncated
        };
        if repeat_notes {
            let earlier = sven_model::repeat::earlier_identical_results(
                replayed,
                &record.name,
                &record.arguments,
                &stored,
            );
            if earlier > 0 {
                stored.push_str(&sven_model::repeat::repeat_note(earlier));
            }
        }
        if without_harness_values(&stored) != without_harness_values(recorded) {
            return Err(ForkError::ResultMismatch {
                call_id: record.id.clone(),
                tool: record.name.clone(),
                recorded: recorded.to_string(),
                replayed: stored,
            });
        }
        Ok(stored)
    }

    /// The tools a replayed session would run: the built-in set this engine's
    /// toolset names, then the engine's own tools over them, all confined to
    /// `root`.
    fn replay_tools(&self, root: &Path) -> Result<ToolRegistry, ForkError> {
        let confined = PathScope::confined(root).map_err(|e| ForkError::Workspace {
            path: root.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, e.to_string()),
        })?;
        let runtime = AgentRuntimeContext {
            project_root: Some(root.to_path_buf()),
            path_scope: confined,
            ..AgentRuntimeContext::default()
        };
        let (events, _) = tokio::sync::mpsc::channel(64);
        let todos = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let buffer = Arc::new(tokio::sync::Mutex::new(OutputBufferStore::new()));
        let profile = ToolSetProfile::for_selection(
            self.toolset().builtin(),
            AgentMode::Agent,
            Some(root),
            None,
            false,
            todos,
            buffer,
        );
        let config = self.config();
        let mut registry = match profile {
            Some(profile) => build_tool_registry(
                &config,
                self.provider().ok_or(ForkError::NoProvider)?,
                profile,
                Arc::new(tokio::sync::Mutex::new(AgentMode::Agent)),
                events,
                runtime,
            ),
            None => ToolRegistry::new(),
        };
        for tool in self.tools() {
            registry.register_arc(tool);
        }
        Ok(registry)
    }
}

/// Splits a recorded transcript into the system prompt it ran under, if it
/// recorded one, and the messages that continue the conversation.
fn split_transcript(transcript: &[Turn]) -> Result<(Option<String>, Vec<Message>), ForkError> {
    let mut role = None;
    let mut history = Vec::new();
    for (at, turn) in transcript.iter().enumerate() {
        match turn {
            Turn::System { text } if at == 0 => role = Some(text.clone()),
            Turn::System { .. } => {
                return Err(ForkError::Unrepresentable {
                    reason: "a system turn outside the first is not a conversation step".into(),
                })
            }
            Turn::User { text } => history.push(Message::user(text)),
            Turn::Assistant { text, tool_calls } => {
                if !text.is_empty() {
                    history.push(Message::assistant(text));
                }
                for record in tool_calls {
                    history.push(tool_call_message(record));
                }
                if text.is_empty() && tool_calls.is_empty() {
                    history.push(Message::assistant(""));
                }
            }
            Turn::ToolResult { call_id, text } => {
                history.push(Message::tool_result(call_id, text));
            }
            Turn::Opaque { role } => {
                return Err(ForkError::Unrepresentable {
                    reason: format!("a {role} turn holds content a conversation cannot hold"),
                })
            }
        }
    }
    Ok((role, history))
}

/// A recorded tool call as the assistant message the kernel stores.
fn tool_call_message(record: &ToolCallRecord) -> Message {
    Message {
        role: Role::Assistant,
        content: MessageContent::ToolCall {
            tool_call_id: record.id.clone(),
            function: sven_model::FunctionCall {
                name: record.name.clone(),
                arguments: record.arguments.clone(),
            },
        },
    }
}

#[cfg(test)]
mod harness_value_tests {
    use super::without_harness_values as n;

    #[test]
    fn the_sandbox_entries_of_a_listing_are_the_same_observation_in_another_attempt() {
        // The observation of `ls -la` in a workspace root, recorded in one attempt
        // and replayed in a fresh one: the student's own files are identical, and
        // what differs belongs to the sandbox - the clock, the attempt's temporary
        // directory, the `.sven` symlink that points into it and its target's
        // length, and the link count of `..`, which counts the entries the
        // sandbox makes beside the workspace.
        let recorded = "total 16\ndrwx------+       4 user user 4096 Oct 24 19:26 .\ndrwx------+       6 user user 4096 Oct 24 19:26 ..\nlrwxrwxrwx+       1 user user   41 Oct 24 19:26 .sven -> /sandbox/attempt-a/.tmpAAAAAA/state\n-rw-------+       1 user user 4365 Jan  1  1970 README.md\ndrwx------+       2 user user 4096 Jan  1  1970 tests";
        let replayed = "total 16\ndrwx------+       4 user user 4096 Oct 25 18:31 .\ndrwx------+       4 user user 4096 Oct 25 18:31 ..\nlrwxrwxrwx+       1 user user   43 Oct 25 18:31 .sven -> /sandbox/attempt-b/.tmpBBBBBB/state\n-rw-------+       1 user user 4365 Jan  1  1970 README.md\ndrwx------+       2 user user 4096 Jan  1  1970 tests";
        assert_eq!(n(recorded), n(replayed));
    }

    #[test]
    fn the_block_total_of_a_copy_of_the_same_tree_is_not_compared() {
        // A real refusal: two listings of the same directory by the same `ls -la`,
        // differing in the total, because the tree was copied rather than written.
        let recorded = "src:\ntotal 28\ndrwx------+  4 user user 4096 Oct 24 19:26 ..\n-rw-------+  1 user user 4289 Jan  1  1970 graph.lua";
        let replayed = "src:\ntotal 8\ndrwx------+  4 user user 4096 Oct 25 18:55 ..\n-rw-------+  1 user user 4289 Jan  1  1970 graph.lua";
        assert_eq!(n(recorded), n(replayed));
        // The entries themselves are still compared.
        assert_ne!(
            n(recorded),
            n("src:\ntotal 8\ndrwx------+  4 user user 4096 Oct 25 18:55 ..\n-rw-------+  1 user user 4290 Jan  1  1970 graph.lua")
        );
    }

    #[test]
    fn a_traceback_from_another_attempt_names_the_same_file() {
        // A real refusal: the student's own test run raised in the workspace,
        // and only the attempt's temporary directory differs.
        let recorded = "Traceback (most recent call last):\n  File \"<stdin>\", line 7, in <module>\n  File \"/sandbox/attempt-a/.tmpAAAAAA/tree/store/memo.py\", line 34, in __init__\n    raise ValueError(\"size must be at least 1\")\nValueError: size must be at least 1";
        let replayed = "Traceback (most recent call last):\n  File \"<stdin>\", line 7, in <module>\n  File \"/sandbox/attempt-b/.tmpBBBBBB/tree/store/memo.py\", line 34, in __init__\n    raise ValueError(\"size must be at least 1\")\nValueError: size must be at least 1";
        assert_eq!(n(recorded), n(replayed));
        // The file the traceback names is still compared.
        assert_ne!(
            n(recorded),
            n(&replayed.replace("store/memo.py\", line 34", "store/cache.py\", line 34"))
        );
    }

    #[test]
    fn the_note_the_harness_adds_to_a_repeated_result_is_not_the_observation() {
        let recorded = "ValueError: size must be at least 1\n\n[exit code 1]";
        let replayed = "ValueError: size must be at least 1\n\n[exit code 1]\n\n[sven note: this exact call returned this exact result once before in this run, and the result is unchanged. Repeating it will not give you new information; take a different action.]";
        assert_eq!(n(recorded), n(replayed));
        assert_ne!(
            n(recorded),
            n("ValueError: size must be at least 2\n\n[exit code 1]")
        );
    }

    #[test]
    fn a_changed_student_entry_beside_the_sandbox_entries_is_still_not_normalised_away() {
        let a = "total 16\ndrwx------+ 4 user user 4096 Oct 24 19:26 .\n-rw-------+ 1 user user 4365 Jan  1  1970 README.md";
        assert_ne!(
            n(a),
            n("total 16\ndrwx------+ 4 user user 4096 Oct 25 18:31 .\n-rw-------+ 1 user user 4366 Jan  1  1970 README.md")
        );
        assert_ne!(
            n(a),
            n("total 16\ndrwx------+ 4 user user 4096 Oct 25 18:31 .\n-rw-------+ 1 user user 4365 Jan  1  1970 README.txt")
        );
    }

    #[test]
    fn a_listing_from_another_attempt_and_another_day_is_the_same_observation() {
        let recorded = "lrwxrwxrwx+   1 user user   41 Oct 24 19:26 .sven -> /sandbox/attempt-a/.tmpAAAAAA/state\n-rw-------+ 1 user user 4365 Jan  1  1970 README.md";
        let replayed = "lrwxrwxrwx+ 1 user user 41 Oct 25 18:19 .sven -> /sandbox/attempt-b/.tmpBBBBBB/state\n-rw-------+ 1 user user 4365 Jan  1  1970 README.md";
        assert_eq!(n(recorded), n(replayed));
    }

    #[test]
    fn a_changed_name_size_or_sentence_is_not_normalised_away() {
        let a = "-rw-------+ 1 user user 4365 Jan  1  1970 README.md";
        assert_ne!(
            n(a),
            n("-rw-------+ 1 user user 4366 Jan  1  1970 README.md")
        );
        assert_ne!(
            n(a),
            n("-rw-------+ 1 user user 4365 Jan  1  1970 README.txt")
        );
        assert_ne!(
            n("all seven symptoms behave correctly"),
            n("all six symptoms behave correctly")
        );
    }

    #[test]
    fn text_without_a_temporary_path_or_a_month_is_borrowed_unchanged() {
        assert!(matches!(n("3 passed"), std::borrow::Cow::Borrowed(_)));
    }
}
