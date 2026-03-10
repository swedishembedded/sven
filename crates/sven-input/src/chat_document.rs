// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! YAML-based chat document format for full-fidelity conversation persistence.
//!
//! Each chat is stored as a single `.yaml` file in `~/.local/share/sven/chats/`.
//! The YAML format is human-readable and human-editable: multi-line strings use
//! block scalars (`|`), tool arguments are native YAML maps, and the document
//! structure mirrors the natural conversation flow.
//!
//! # File format example
//!
//! ```yaml
//! id: "01JQ8KPXYZ..."
//! title: "Debug hard fault in PMIC driver"
//! created_at: "2026-03-09T11:37:29Z"
//! updated_at: "2026-03-09T12:05:43Z"
//! model: "anthropic/claude-sonnet-4-20250514"
//! mode: code
//! status: completed
//! turns:
//!   - role: user
//!     content: |
//!       Use gdb to debug the hard fault.
//!   - role: assistant
//!     content: |
//!       I'll help debug using GDB.
//!   - role: tool_call
//!     tool_call_id: "toolu_01E9eMNfm8"
//!     name: list_dir
//!     arguments:
//!       depth: 2
//!       path: /data/ng-iot-platform
//!   - role: tool_result
//!     tool_call_id: "toolu_01E9eMNfm8"
//!     content: |
//!       src/
//!       pmic/
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sven_model::{FunctionCall, Message, MessageContent, Role};
use uuid::Uuid;

use crate::conversation::ConversationRecord;

// ── SessionId ─────────────────────────────────────────────────────────────────

/// Opaque identifier for a chat session, backed by a UUID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(String);

impl SessionId {
    /// Generate a new random session ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Wrap an existing string as a session ID (for loading from YAML).
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    /// Borrow the inner string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── ChatStatus ────────────────────────────────────────────────────────────────

/// Lifecycle status of a chat session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatStatus {
    /// Session is currently open and may be active.
    #[default]
    Active,
    /// Session completed normally (agent turn finished, no pending input).
    Completed,
    /// Session has been archived by the user.
    Archived,
}

// ── TurnRecord ────────────────────────────────────────────────────────────────

/// A single record in the conversation turn list.
///
/// Uses the `role` field as a YAML tag discriminant (internally tagged).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum TurnRecord {
    /// A message from the human user.
    User { content: String },

    /// A text response from the AI assistant.
    Assistant { content: String },

    /// A reasoning/thinking block produced by the model.
    Thinking { content: String },

    /// A tool call initiated by the assistant.
    ToolCall {
        tool_call_id: String,
        name: String,
        /// Arguments as a native YAML map for human editability.
        arguments: serde_yaml::Value,
    },

    /// The result returned by a tool.
    ToolResult {
        tool_call_id: String,
        content: String,
    },

    /// A marker recording that context was compacted to save tokens.
    ContextCompacted {
        tokens_before: usize,
        tokens_after: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strategy: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u32>,
    },
}

// ── ChatDocument ──────────────────────────────────────────────────────────────

/// Full chat document — the canonical persistence format for a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatDocument {
    /// Unique session identifier.
    pub id: SessionId,
    /// Human-readable title, generated from first user message or model API.
    pub title: String,
    /// Model used for this conversation (e.g. "anthropic/claude-sonnet-4-20250514").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Agent mode used (e.g. "agent", "code", "research").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Session lifecycle status.
    #[serde(default)]
    pub status: ChatStatus,
    /// When this document was first created (UTC).
    pub created_at: DateTime<Utc>,
    /// When this document was last saved (UTC).
    pub updated_at: DateTime<Utc>,
    /// All turns in conversation order.
    #[serde(default)]
    pub turns: Vec<TurnRecord>,
}

impl ChatDocument {
    /// Create a blank document with a new session ID and no turns.
    pub fn new(title: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: SessionId::new(),
            title: title.into(),
            model: None,
            mode: None,
            status: ChatStatus::Active,
            created_at: now,
            updated_at: now,
            turns: Vec::new(),
        }
    }

    /// Touch `updated_at` to the current time.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Number of user/assistant turn pairs.
    pub fn turn_count(&self) -> usize {
        self.turns
            .iter()
            .filter(|t| matches!(t, TurnRecord::User { .. }))
            .count()
    }
}

// ── Serialization ─────────────────────────────────────────────────────────────

/// YAML document start marker. Prepended so saved files are valid multi-document
/// YAML and clearly delimited.
const YAML_DOCUMENT_START: &str = "---\n";

/// Serialize a `ChatDocument` to a YAML string.
///
/// Uses `serde_yaml` which automatically formats multi-line strings as
/// YAML block scalars (`|`) for human readability. Output is prefixed with `---`
/// and uses 2-space indentation for nested structures.
pub fn serialize_chat_document(doc: &ChatDocument) -> Result<String> {
    let raw = serde_yaml::to_string(doc).context("serializing ChatDocument to YAML")?;
    let out = if raw.starts_with("---") {
        raw
    } else {
        format!("{YAML_DOCUMENT_START}{raw}")
    };
    Ok(out)
}

/// Parse a `ChatDocument` from YAML text.
pub fn parse_chat_document(yaml: &str) -> Result<ChatDocument> {
    serde_yaml::from_str(yaml).context("parsing ChatDocument from YAML")
}

// ── Conversion from ConversationRecord ───────────────────────────────────────

/// Convert a slice of `ConversationRecord`s to `TurnRecord`s for embedding in
/// a `ChatDocument`.
///
/// System messages are skipped — the agent regenerates the system prompt at
/// runtime from the current config.
pub fn records_to_turns(records: &[ConversationRecord]) -> Vec<TurnRecord> {
    records.iter().filter_map(record_to_turn).collect()
}

fn record_to_turn(record: &ConversationRecord) -> Option<TurnRecord> {
    match record {
        ConversationRecord::Message(m) => message_to_turn(m),
        ConversationRecord::Thinking { content } => Some(TurnRecord::Thinking {
            content: content.clone(),
        }),
        ConversationRecord::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn,
        } => Some(TurnRecord::ContextCompacted {
            tokens_before: *tokens_before,
            tokens_after: *tokens_after,
            strategy: strategy.clone(),
            turn: *turn,
        }),
    }
}

fn message_to_turn(msg: &Message) -> Option<TurnRecord> {
    match (&msg.role, &msg.content) {
        (Role::System, _) => None, // system messages skipped

        (Role::User, MessageContent::Text(t)) => Some(TurnRecord::User { content: t.clone() }),

        (Role::Assistant, MessageContent::Text(t)) => {
            Some(TurnRecord::Assistant { content: t.clone() })
        }

        (
            Role::Assistant,
            MessageContent::ToolCall {
                tool_call_id,
                function,
            },
        ) => {
            let arguments = json_str_to_yaml(&function.arguments);
            Some(TurnRecord::ToolCall {
                tool_call_id: tool_call_id.clone(),
                name: function.name.clone(),
                arguments,
            })
        }

        (
            Role::Tool,
            MessageContent::ToolResult {
                tool_call_id,
                content,
            },
        ) => Some(TurnRecord::ToolResult {
            tool_call_id: tool_call_id.clone(),
            content: content.to_string(),
        }),

        _ => None,
    }
}

// ── Conversion to ConversationRecord / Message ────────────────────────────────

/// Convert `TurnRecord`s from a `ChatDocument` to `ConversationRecord`s for
/// JSONL export or history replay.
pub fn turns_to_records(turns: &[TurnRecord]) -> Vec<ConversationRecord> {
    turns.iter().filter_map(turn_to_record).collect()
}

fn turn_to_record(turn: &TurnRecord) -> Option<ConversationRecord> {
    match turn {
        TurnRecord::User { content } => Some(ConversationRecord::Message(Message::user(content))),
        TurnRecord::Assistant { content } => {
            Some(ConversationRecord::Message(Message::assistant(content)))
        }
        TurnRecord::Thinking { content } => Some(ConversationRecord::Thinking {
            content: content.clone(),
        }),
        TurnRecord::ToolCall {
            tool_call_id,
            name,
            arguments,
        } => {
            let args_json = yaml_to_json_str(arguments);
            Some(ConversationRecord::Message(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: tool_call_id.clone(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: args_json,
                    },
                },
            }))
        }
        TurnRecord::ToolResult {
            tool_call_id,
            content,
        } => Some(ConversationRecord::Message(Message::tool_result(
            tool_call_id.clone(),
            content,
        ))),
        TurnRecord::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn,
        } => Some(ConversationRecord::ContextCompacted {
            tokens_before: *tokens_before,
            tokens_after: *tokens_after,
            strategy: strategy.clone(),
            turn: *turn,
        }),
    }
}

/// Convert `TurnRecord`s to `Message`s suitable for seeding an `Agent`.
///
/// System messages are never included; `ContextCompacted` and `Thinking`
/// entries are skipped (the agent reconstructs them during the run).
pub fn turns_to_messages(turns: &[TurnRecord]) -> Vec<Message> {
    turns.iter().filter_map(turn_to_message).collect()
}

fn turn_to_message(turn: &TurnRecord) -> Option<Message> {
    match turn {
        TurnRecord::User { content } => Some(Message::user(content)),
        TurnRecord::Assistant { content } => Some(Message::assistant(content)),
        TurnRecord::Thinking { .. } => None, // thinking not sent back to model
        TurnRecord::ToolCall {
            tool_call_id,
            name,
            arguments,
        } => {
            let args_json = yaml_to_json_str(arguments);
            Some(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: tool_call_id.clone(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: args_json,
                    },
                },
            })
        }
        TurnRecord::ToolResult {
            tool_call_id,
            content,
        } => Some(Message::tool_result(tool_call_id.clone(), content)),
        TurnRecord::ContextCompacted { .. } => None,
    }
}

// ── Chat directory and file operations ───────────────────────────────────────

/// Returns the directory where sven stores chat documents.
///
/// Defaults to `$XDG_DATA_HOME/sven/chats` (i.e. `~/.local/share/sven/chats`).
pub fn chat_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        })
        .join("sven")
        .join("chats")
}

/// Creates the chat directory if it does not exist and returns its path.
pub fn ensure_chat_dir() -> Result<PathBuf> {
    let dir = chat_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating chat directory {}", dir.display()))?;
    Ok(dir)
}

/// Returns the file path for a chat document with the given session ID.
pub fn chat_path(id: &SessionId) -> PathBuf {
    chat_dir().join(format!("{}.yaml", id))
}

/// Save a `ChatDocument` to its canonical path in the chat directory.
pub fn save_chat(doc: &mut ChatDocument) -> Result<()> {
    doc.touch();
    let dir = ensure_chat_dir()?;
    let path = dir.join(format!("{}.yaml", doc.id));
    let content = serialize_chat_document(doc)?;
    std::fs::write(&path, content)
        .with_context(|| format!("writing chat document to {}", path.display()))
}

/// Save a `ChatDocument` to a specific path.
pub fn save_chat_to(path: &Path, doc: &mut ChatDocument) -> Result<()> {
    doc.touch();
    let content = serialize_chat_document(doc)?;
    std::fs::write(path, content)
        .with_context(|| format!("writing chat document to {}", path.display()))
}

// ── Atomic save with modification detection ───────────────────────────────────

/// Error returned when the file was modified after we read it but before we
/// tried to write, indicating a concurrent modification.
#[derive(Debug, thiserror::Error)]
#[error("file was modified by another process")]
pub struct FileModifiedError;

impl From<anyhow::Error> for FileModifiedError {
    fn from(_: anyhow::Error) -> Self {
        FileModifiedError
    }
}
/// Save a `ChatDocument` atomically, checking for concurrent modifications.
///
/// Atomicity is enforced by the kernel:
/// 1. Prepare new content and write it to a temp file.
/// 2. Take an exclusive `flock` on a lock file (same directory as the chat file).
/// 3. While holding the lock, check that the target file is unchanged (ino/mtime).
/// 4. Replace the target with the temp file via a single `rename()` (atomic on POSIX).
/// 5. Release the lock.
///
/// The lock ensures no other writer can change the file between our check and
/// rename, so we never overwrite a file that has changed.
///
/// Returns `Err(FileModifiedError)` if the file was modified by another
/// process after we read it.
pub fn save_chat_atomic(doc: &mut ChatDocument) -> Result<(), FileModifiedError> {
    let dir = ensure_chat_dir().map_err(|e| anyhow::anyhow!("{}", e))?;
    let path = dir.join(format!("{}.yaml", doc.id));
    save_chat_to_atomic(&path, doc)
}

/// Save a `ChatDocument` to a specific path atomically, checking for concurrent
/// modifications. See [`save_chat_atomic`] for details.
pub fn save_chat_to_atomic(path: &Path, doc: &mut ChatDocument) -> Result<(), FileModifiedError> {
    use std::os::unix::fs::MetadataExt;

    doc.touch();
    let content = serialize_chat_document(doc).map_err(|e| anyhow::anyhow!("{}", e))?;

    // Snapshot of target metadata before we prepare the write (no lock yet)
    let initial_metadata = match std::fs::metadata(path) {
        Ok(m) => Some((m.ino(), m.mtime())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(FileModifiedError),
    };

    // Write to a temporary file in the same directory (rename must be same fs)
    let temp_path = path.with_extension("yaml.tmp");
    std::fs::write(&temp_path, &content).map_err(|_| FileModifiedError)?;

    // From here on, check and replace must be atomic: hold exclusive lock so
    // no other writer can change the file between our stat and rename.
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let lock_path = path.with_extension("lock");
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&lock_path)
            .map_err(|_| FileModifiedError)?;
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            let _ = std::fs::remove_file(&temp_path);
            return Err(FileModifiedError);
        }
        let _guard = LockGuard(lock_file);
        // With lock held: we are the only writer; target cannot change until we unlock.
        if let Some((initial_ino, initial_mtime)) = initial_metadata {
            match std::fs::metadata(path) {
                Ok(m) => {
                    if m.ino() != initial_ino || m.mtime() != initial_mtime {
                        let _ = std::fs::remove_file(&temp_path);
                        return Err(FileModifiedError);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // File was deleted while we held lock (another process with lock removed it)
                    let _ = std::fs::remove_file(&temp_path);
                    return Err(FileModifiedError);
                }
                Err(_) => {
                    let _ = std::fs::remove_file(&temp_path);
                    return Err(FileModifiedError);
                }
            }
        }
        std::fs::rename(&temp_path, path).map_err(|_| FileModifiedError)?;
    }

    #[cfg(not(unix))]
    {
        if let Some((initial_ino, initial_mtime)) = initial_metadata {
            match std::fs::metadata(path) {
                Ok(m) => {
                    if m.ino() != initial_ino || m.mtime() != initial_mtime {
                        let _ = std::fs::remove_file(&temp_path);
                        return Err(FileModifiedError);
                    }
                }
                Err(_) => {}
            }
        }
        std::fs::rename(&temp_path, path).map_err(|_| FileModifiedError)?;
    }

    Ok(())
}

#[cfg(unix)]
struct LockGuard(std::fs::File);

#[cfg(unix)]
impl Drop for LockGuard {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Load a `ChatDocument` along with its file metadata for later atomic write
/// verification. The metadata can be passed to [`save_chat_with_metadata`] to
/// detect concurrent modifications.
pub fn load_chat_with_metadata(id: &SessionId) -> Result<(ChatDocument, FileMetadata)> {
    let path = chat_path(id);
    load_chat_from_with_metadata(&path)
}

/// Load a `ChatDocument` from an explicit file path along with its metadata.
pub fn load_chat_from_with_metadata(path: &Path) -> Result<(ChatDocument, FileMetadata)> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("reading chat document metadata {}", path.display()))?;
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading chat document {}", path.display()))?;
    let doc = parse_chat_document(&content)
        .with_context(|| format!("parsing chat document {}", path.display()))?;
    Ok((doc, FileMetadata::from(metadata)))
}

/// File metadata used for detecting concurrent modifications.
#[derive(Debug, Clone)]
pub struct FileMetadata {
    inode: u64,
    mtime: i64,
}

impl FileMetadata {
    /// Check if the file has been modified since this metadata was captured.
    pub fn is_modified(&self, path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(path) {
            Ok(m) => m.ino() != self.inode || m.mtime() != self.mtime,
            Err(_) => true, // File doesn't exist or can't be read = modified
        }
    }
}

impl From<std::fs::Metadata> for FileMetadata {
    fn from(m: std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            inode: m.ino(),
            mtime: m.mtime(),
        }
    }
}

/// Load a `ChatDocument` from its canonical path using the session ID.
pub fn load_chat(id: &SessionId) -> Result<ChatDocument> {
    let path = chat_path(id);
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading chat document {}", path.display()))?;
    parse_chat_document(&content)
        .with_context(|| format!("parsing chat document {}", path.display()))
}

/// Load a `ChatDocument` from an explicit file path.
pub fn load_chat_from(path: &Path) -> Result<ChatDocument> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading chat document {}", path.display()))?;
    parse_chat_document(&content)
        .with_context(|| format!("parsing chat document {}", path.display()))
}

/// Summary of a chat shown when listing chats.
#[derive(Debug, Clone)]
pub struct ChatEntry {
    /// Session ID (also the file stem).
    pub id: SessionId,
    /// Full path to the YAML file.
    pub path: PathBuf,
    /// Human-readable title.
    pub title: String,
    /// Number of user turns.
    pub turns: usize,
    /// When the document was last updated.
    pub updated_at: DateTime<Utc>,
    /// Session status.
    pub status: ChatStatus,
}

/// List all chat documents in the chat directory, most recently updated first.
pub fn list_chats(limit: Option<usize>) -> Result<Vec<ChatEntry>> {
    let dir = chat_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&dir).context("reading chat directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| parse_chat_document(&s).ok())
        {
            Some(doc) => {
                let turns = doc.turn_count();
                entries.push(ChatEntry {
                    id: doc.id,
                    path,
                    title: doc.title,
                    turns,
                    updated_at: doc.updated_at,
                    status: doc.status,
                });
            }
            None => {
                // Unreadable / malformed — skip with a warning.
                tracing::warn!(path = %path.display(), stem = %stem, "skipping malformed chat document");
            }
        }
    }

    entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    if let Some(n) = limit {
        entries.truncate(n);
    }
    Ok(entries)
}

// ── YAML ↔ JSON conversion helpers ───────────────────────────────────────────

/// Convert a JSON-encoded argument string to a `serde_yaml::Value` map.
///
/// Used when converting tool-call records to YAML for human-editable storage.
/// Falls back to an empty mapping on any parse error.
pub fn json_str_to_yaml(json_str: &str) -> serde_yaml::Value {
    serde_json::from_str::<serde_json::Value>(json_str)
        .ok()
        .and_then(|v| serde_yaml::to_value(v).ok())
        .unwrap_or_else(|| serde_yaml::Value::Mapping(Default::default()))
}

/// Convert a `serde_yaml::Value` to a compact JSON string.
///
/// Used when converting YAML tool-call arguments back to the JSON string that
/// `FunctionCall::arguments` expects.
pub fn yaml_to_json_str(val: &serde_yaml::Value) -> String {
    serde_yaml::from_value::<serde_json::Value>(val.clone())
        .ok()
        .and_then(|v| serde_json::to_string(&v).ok())
        .unwrap_or_else(|| "{}".to_string())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_doc() -> ChatDocument {
        let mut doc = ChatDocument::new("Test conversation");
        doc.model = Some("anthropic/claude-3-5".to_string());
        doc.turns = vec![
            TurnRecord::User {
                content: "Hello, how are you?".to_string(),
            },
            TurnRecord::Assistant {
                content: "I'm doing well, thank you!".to_string(),
            },
        ];
        doc
    }

    #[test]
    fn round_trip_simple() {
        let doc = make_doc();
        let yaml = serialize_chat_document(&doc).unwrap();
        let parsed = parse_chat_document(&yaml).unwrap();
        assert_eq!(parsed.title, doc.title);
        assert_eq!(parsed.turns.len(), 2);
        match &parsed.turns[0] {
            TurnRecord::User { content } => assert_eq!(content, "Hello, how are you?"),
            _ => panic!("expected User"),
        }
        match &parsed.turns[1] {
            TurnRecord::Assistant { content } => assert_eq!(content, "I'm doing well, thank you!"),
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn round_trip_tool_call() {
        let mut doc = ChatDocument::new("Tool test");
        doc.turns = vec![
            TurnRecord::User {
                content: "List files".to_string(),
            },
            TurnRecord::ToolCall {
                tool_call_id: "call_001".to_string(),
                name: "list_dir".to_string(),
                arguments: json_str_to_yaml(r#"{"path":"/tmp","depth":2}"#),
            },
            TurnRecord::ToolResult {
                tool_call_id: "call_001".to_string(),
                content: "file1.rs\nfile2.rs\n".to_string(),
            },
            TurnRecord::Assistant {
                content: "Found 2 Rust files.".to_string(),
            },
        ];

        let yaml = serialize_chat_document(&doc).unwrap();
        let parsed = parse_chat_document(&yaml).unwrap();
        assert_eq!(parsed.turns.len(), 4);

        match &parsed.turns[1] {
            TurnRecord::ToolCall {
                tool_call_id,
                name,
                arguments,
            } => {
                assert_eq!(tool_call_id, "call_001");
                assert_eq!(name, "list_dir");
                // Arguments round-trip through YAML and back to JSON
                let json = yaml_to_json_str(arguments);
                let v: serde_json::Value = serde_json::from_str(&json).unwrap();
                assert_eq!(v["path"], "/tmp");
                assert_eq!(v["depth"], 2);
            }
            _ => panic!("expected ToolCall"),
        }

        match &parsed.turns[2] {
            TurnRecord::ToolResult {
                tool_call_id,
                content,
            } => {
                assert_eq!(tool_call_id, "call_001");
                assert!(content.contains("file1.rs"));
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn round_trip_thinking_and_compacted() {
        let mut doc = ChatDocument::new("Thinking test");
        doc.turns = vec![
            TurnRecord::User {
                content: "What is 2+2?".to_string(),
            },
            TurnRecord::Thinking {
                content: "The user wants to know 2+2. That is 4.".to_string(),
            },
            TurnRecord::Assistant {
                content: "4".to_string(),
            },
            TurnRecord::ContextCompacted {
                tokens_before: 1000,
                tokens_after: 100,
                strategy: Some("structured".to_string()),
                turn: Some(3),
            },
        ];

        let yaml = serialize_chat_document(&doc).unwrap();
        let parsed = parse_chat_document(&yaml).unwrap();
        assert_eq!(parsed.turns.len(), 4);

        match &parsed.turns[3] {
            TurnRecord::ContextCompacted {
                tokens_before,
                tokens_after,
                strategy,
                turn,
            } => {
                assert_eq!(*tokens_before, 1000);
                assert_eq!(*tokens_after, 100);
                assert_eq!(strategy.as_deref(), Some("structured"));
                assert_eq!(*turn, Some(3));
            }
            _ => panic!("expected ContextCompacted"),
        }
    }

    #[test]
    fn turns_to_messages_skips_thinking_and_compacted() {
        let turns = vec![
            TurnRecord::User {
                content: "Do task".to_string(),
            },
            TurnRecord::Thinking {
                content: "reasoning".to_string(),
            },
            TurnRecord::Assistant {
                content: "Done.".to_string(),
            },
            TurnRecord::ContextCompacted {
                tokens_before: 500,
                tokens_after: 50,
                strategy: None,
                turn: None,
            },
        ];
        let messages = turns_to_messages(&turns);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].as_text(), Some("Do task"));
        assert_eq!(messages[1].as_text(), Some("Done."));
    }

    #[test]
    fn records_to_turns_and_back() {
        let records = vec![
            ConversationRecord::Message(Message::user("Hello")),
            ConversationRecord::Message(Message::assistant("Hi")),
            ConversationRecord::Thinking {
                content: "thinking".to_string(),
            },
        ];
        let turns = records_to_turns(&records);
        assert_eq!(turns.len(), 3);
        let back = turns_to_records(&turns);
        assert_eq!(back.len(), 3);
    }

    #[test]
    fn system_messages_skipped_in_conversion() {
        let records = vec![
            ConversationRecord::Message(Message::system("You are sven.")),
            ConversationRecord::Message(Message::user("Hello")),
            ConversationRecord::Message(Message::assistant("Hi")),
        ];
        let turns = records_to_turns(&records);
        assert_eq!(turns.len(), 2, "system message must be skipped");
        match &turns[0] {
            TurnRecord::User { content } => assert_eq!(content, "Hello"),
            _ => panic!("expected User"),
        }
    }

    #[test]
    fn session_id_display() {
        let id = SessionId::new();
        assert!(!id.as_str().is_empty());
        let displayed = format!("{}", id);
        assert_eq!(displayed, id.as_str());
    }

    #[test]
    fn turn_count() {
        let doc = make_doc();
        assert_eq!(doc.turn_count(), 1);
    }

    #[test]
    fn multiline_content_preserved() {
        let mut doc = ChatDocument::new("Multiline");
        let long = "Line one.\nLine two.\nLine three.";
        doc.turns = vec![TurnRecord::User {
            content: long.to_string(),
        }];
        let yaml = serialize_chat_document(&doc).unwrap();
        let parsed = parse_chat_document(&yaml).unwrap();
        match &parsed.turns[0] {
            TurnRecord::User { content } => assert_eq!(content, long),
            _ => panic!("expected User"),
        }
    }
}
