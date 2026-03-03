// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Local conversation store — append-only JSONL files on disk.
//!
//! # One conversation per peer pair
//!
//! There is exactly one conversation file per remote peer:
//!
//! ```text
//! ~/.config/sven/conversations/
//! ├── peers/
//! │   ├── 12D3KooWAbc….jsonl   ← all messages with this peer, oldest first
//! │   └── 12D3KooWXyz….jsonl
//! └── rooms/
//!     ├── firmware-team.jsonl
//!     └── general.jsonl
//! ```
//!
//! Each line is a self-contained JSON object (JSONL).
//!
//! # Context breaks
//!
//! A *break* is a gap between two consecutive messages in the same conversation
//! that exceeds a threshold (default 1 hour).  When loading context for an
//! inbound message, the store finds the most recent break and returns only the
//! messages after it.  This keeps the context window focused on the current
//! exchange.  To recall information from before the break the agent uses the
//! `search_conversation` tool.
//!
//! # Search
//!
//! Full-text search uses a [`regex::Regex`] pattern applied to every `Text`
//! content block — exactly like `grep`.  The pattern is applied case-sensitively
//! by default; callers can pass a `(?i)` prefix for case-insensitive matching.
//!
//! # Thread safety
//!
//! `ConversationStore` is `Send + Sync`.  All file I/O is synchronous (blocking
//! `std::fs`).  Callers in async contexts wrap calls with
//! `tokio::task::spawn_blocking`.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::protocol::types::{ContentBlock, SessionRole};

// ── Record types ─────────────────────────────────────────────────────────────

/// One message in a peer conversation log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationRecord {
    /// Unique message ID — used for deduplication.
    pub message_id: Uuid,
    /// Monotonically increasing position in this peer's conversation log.
    pub seq: u64,
    /// When this message was created.
    pub timestamp: DateTime<Utc>,
    /// `"inbound"` or `"outbound"` from the perspective of the local node.
    pub direction: MessageDirection,
    /// Base-58 peer ID of the *remote* participant.
    pub peer_id: String,
    /// Conversational role of the author.
    pub role: SessionRole,
    /// Multimodal content.
    pub content: Vec<ContentBlock>,
    /// Session-chain hop depth from the wire message.
    ///
    /// Used by `WaitForMessageTool` to propagate the received depth back into
    /// the task agent's `SendMessageTool` so that subsequent sends continue
    /// incrementing from the correct depth rather than restarting from zero.
    ///
    /// Defaults to `0` when loading old records from disk that predate this
    /// field (local storage compat — no network implication).
    #[serde(default)]
    pub depth: u32,
}

/// One post in a room history log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomRecord {
    pub message_id: Uuid,
    pub room: String,
    pub sender_peer_id: String,
    pub sender_name: String,
    pub timestamp: DateTime<Utc>,
    pub content: Vec<ContentBlock>,
    /// Reactive-response hop depth carried from the corresponding [`RoomPost`].
    ///
    /// Exposed by `ReadRoomHistoryTool` so that the LLM can pass it as
    /// `in_reply_to_depth` when calling `PostToRoomTool`, propagating the reply
    /// chain depth correctly and preventing proactive reply loops.
    ///
    /// Defaults to `0` for records persisted before this field was added
    /// (`#[serde(default)]` ensures backward-compatible deserialization).
    #[serde(default)]
    pub depth: u32,
}

/// Direction of a message relative to the local node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDirection {
    Inbound,
    Outbound,
}

// ── Store ─────────────────────────────────────────────────────────────────────

/// Thread-safe, append-only local conversation store.
///
/// All methods are synchronous (blocking I/O).
#[derive(Debug, Clone)]
pub struct ConversationStore {
    base_dir: PathBuf,
}

pub type ConversationStoreHandle = Arc<ConversationStore>;

/// The default gap between two messages that signals a conversation break.
pub const DEFAULT_BREAK_THRESHOLD: Duration = Duration::from_secs(3600); // 1 hour

impl ConversationStore {
    /// Create a store rooted at `base_dir`.
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Default base directory: `~/.config/sven/conversations/`.
    pub fn default_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config/sven/conversations")
    }

    // ── Peer conversation methods ─────────────────────────────────────────────

    /// Append one outbound or inbound message to the peer's conversation log.
    pub fn append_message(&self, record: &ConversationRecord) -> anyhow::Result<()> {
        let path = self.peer_file_path(&record.peer_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(record)? + "\n";
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }

    /// Load all messages in a peer's conversation log (oldest first).
    pub fn load_all(&self, peer_id: &str) -> anyhow::Result<Vec<ConversationRecord>> {
        let path = self.peer_file_path(peer_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        self.read_peer_file(&path)
    }

    /// Load only the messages after the most recent break (gap ≥ `threshold`).
    ///
    /// A *break* is the largest gap between two consecutive messages.  If there
    /// has been no break since the conversation began, all messages are returned.
    ///
    /// This is the slice used as LLM context when handling an inbound message.
    pub fn load_context_after_break(
        &self,
        peer_id: &str,
        threshold: Duration,
    ) -> anyhow::Result<Vec<ConversationRecord>> {
        let records = self.load_all(peer_id)?;
        if records.is_empty() {
            return Ok(Vec::new());
        }
        // Walk backwards through consecutive pairs to find the last break.
        let mut break_idx = 0usize; // index of first record *after* the break
        for i in (1..records.len()).rev() {
            let gap = records[i]
                .timestamp
                .signed_duration_since(records[i - 1].timestamp)
                .to_std()
                .unwrap_or(Duration::ZERO);
            if gap >= threshold {
                break_idx = i;
                break;
            }
        }
        Ok(records[break_idx..].to_vec())
    }

    /// Count messages in the conversation with a peer (used for seq assignment).
    pub fn message_count(&self, peer_id: &str) -> anyhow::Result<u64> {
        let path = self.peer_file_path(peer_id);
        if !path.exists() {
            return Ok(0);
        }
        let file = fs::File::open(&path)?;
        let reader = BufReader::new(file);
        let count = reader
            .lines()
            .filter(|l| l.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false))
            .count();
        Ok(count as u64)
    }

    /// Regex search across one peer's conversation (or all peers).
    ///
    /// Applies `pattern` to every `Text` content block, just like `grep`.
    /// Returns matching records in chronological order, capped at `limit`.
    ///
    /// # Pattern syntax
    ///
    /// Full Rust [`regex`](https://docs.rs/regex) syntax.  Use `(?i)` prefix
    /// for case-insensitive matching.
    ///
    /// # Errors
    ///
    /// Returns an error if `pattern` is not a valid regex.
    pub fn search(
        &self,
        peer_id: Option<&str>,
        pattern: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<ConversationRecord>> {
        let re = Regex::new(pattern)?;
        let peers_dir = self.base_dir.join("peers");
        if !peers_dir.exists() {
            return Ok(Vec::new());
        }

        let files: Vec<PathBuf> = if let Some(pid) = peer_id {
            let f = self.peer_file_path(pid);
            if f.exists() {
                vec![f]
            } else {
                vec![]
            }
        } else {
            fs::read_dir(&peers_dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
                .collect()
        };

        let mut results = Vec::new();
        'outer: for file in &files {
            for record in self.read_peer_file(file)? {
                if record_matches_regex(&record.content, &re) {
                    results.push(record);
                    if results.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
        Ok(results)
    }

    /// List all peers that have conversation history, with basic metadata.
    pub fn list_peers_with_history(&self) -> anyhow::Result<Vec<PeerHistorySummary>> {
        let peers_dir = self.base_dir.join("peers");
        if !peers_dir.exists() {
            return Ok(Vec::new());
        }
        let mut summaries = Vec::new();
        for entry in fs::read_dir(&peers_dir)?.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let peer_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let records = self.read_peer_file(&path)?;
            let first = records.first().map(|r| r.timestamp);
            let last = records.last().map(|r| r.timestamp);
            summaries.push(PeerHistorySummary {
                peer_id,
                message_count: records.len(),
                first_timestamp: first,
                last_timestamp: last,
            });
        }
        summaries.sort_by(|a, b| {
            b.last_timestamp
                .unwrap_or(DateTime::<Utc>::MIN_UTC)
                .cmp(&a.last_timestamp.unwrap_or(DateTime::<Utc>::MIN_UTC))
        });
        Ok(summaries)
    }

    // ── Room methods ──────────────────────────────────────────────────────────

    /// Append one room post to the room's log.
    pub fn append_room_post(&self, record: &RoomRecord) -> anyhow::Result<()> {
        let path = self.room_file_path(&record.room);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(record)? + "\n";
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }

    /// Read room history with optional time filter, regex filter, and limit.
    pub fn read_room_history(
        &self,
        room: &str,
        since: Option<DateTime<Utc>>,
        limit: usize,
        pattern: Option<&str>,
    ) -> anyhow::Result<Vec<RoomRecord>> {
        let re = pattern.map(Regex::new).transpose()?;
        let path = self.room_file_path(room);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let records = self.read_room_file(&path)?;
        let results: Vec<RoomRecord> = records
            .into_iter()
            .filter(|r| since.is_none_or(|s| r.timestamp >= s))
            .filter(|r| {
                re.as_ref()
                    .is_none_or(|re| record_matches_regex(&r.content, re))
            })
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Ok(results)
    }

    // ── Path helpers ──────────────────────────────────────────────────────────

    fn peer_file_path(&self, peer_id: &str) -> PathBuf {
        // Sanitise: keep only chars valid in filenames (base58 IDs are already safe).
        let safe: String = peer_id
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.base_dir.join("peers").join(format!("{safe}.jsonl"))
    }

    fn room_file_path(&self, room: &str) -> PathBuf {
        let safe: String = room
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.base_dir.join("rooms").join(format!("{safe}.jsonl"))
    }

    // ── File readers ──────────────────────────────────────────────────────────

    fn read_peer_file(&self, path: &PathBuf) -> anyhow::Result<Vec<ConversationRecord>> {
        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut records = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ConversationRecord>(&line) {
                Ok(r) => records.push(r),
                Err(e) => tracing::warn!(
                    path = %path.display(), error = %e,
                    "skipping malformed conversation record"
                ),
            }
        }
        Ok(records)
    }

    fn read_room_file(&self, path: &PathBuf) -> anyhow::Result<Vec<RoomRecord>> {
        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut records = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<RoomRecord>(&line) {
                Ok(r) => records.push(r),
                Err(e) => tracing::warn!(
                    path = %path.display(), error = %e,
                    "skipping malformed room record"
                ),
            }
        }
        Ok(records)
    }
}

// ── Summary types ─────────────────────────────────────────────────────────────

/// Summary of the conversation history with one peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerHistorySummary {
    pub peer_id: String,
    pub message_count: usize,
    pub first_timestamp: Option<DateTime<Utc>>,
    pub last_timestamp: Option<DateTime<Utc>>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn record_matches_regex(content: &[ContentBlock], re: &Regex) -> bool {
    content.iter().any(|block| match block {
        ContentBlock::Text { text } => re.is_match(text),
        ContentBlock::Json { value } => re.is_match(&value.to_string()),
        ContentBlock::Image { .. } => false,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, ConversationStore) {
        let dir = TempDir::new().unwrap();
        let store = ConversationStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn msg(
        peer: &str,
        seq: u64,
        text: &str,
        dir: MessageDirection,
        offset_secs: i64,
    ) -> ConversationRecord {
        ConversationRecord {
            message_id: Uuid::new_v4(),
            seq,
            timestamp: DateTime::from_timestamp(1_700_000_000 + offset_secs, 0).unwrap(),
            direction: dir,
            peer_id: peer.to_string(),
            role: SessionRole::User,
            content: vec![ContentBlock::text(text)],
            depth: 0,
        }
    }

    #[test]
    fn append_and_load_all() {
        let (_dir, store) = make_store();
        store
            .append_message(&msg("peer1", 0, "hello", MessageDirection::Outbound, 0))
            .unwrap();
        store
            .append_message(&msg("peer1", 1, "world", MessageDirection::Inbound, 10))
            .unwrap();

        let all = store.load_all("peer1").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].seq, 0);
        assert_eq!(all[1].seq, 1);
    }

    #[test]
    fn load_context_after_break_no_break() {
        let (_dir, store) = make_store();
        // Messages 5 minutes apart — no break.
        store
            .append_message(&msg("peer1", 0, "a", MessageDirection::Outbound, 0))
            .unwrap();
        store
            .append_message(&msg("peer1", 1, "b", MessageDirection::Inbound, 300))
            .unwrap();
        store
            .append_message(&msg("peer1", 2, "c", MessageDirection::Outbound, 600))
            .unwrap();

        let ctx = store
            .load_context_after_break("peer1", Duration::from_secs(3600))
            .unwrap();
        assert_eq!(ctx.len(), 3);
    }

    #[test]
    fn load_context_after_break_with_break() {
        let (_dir, store) = make_store();
        // First block: t=0, t=300
        store
            .append_message(&msg("peer1", 0, "old1", MessageDirection::Outbound, 0))
            .unwrap();
        store
            .append_message(&msg("peer1", 1, "old2", MessageDirection::Inbound, 300))
            .unwrap();
        // Break: 2-hour gap.
        store
            .append_message(&msg("peer1", 2, "new1", MessageDirection::Outbound, 7500))
            .unwrap();
        store
            .append_message(&msg("peer1", 3, "new2", MessageDirection::Inbound, 7800))
            .unwrap();

        let ctx = store
            .load_context_after_break("peer1", Duration::from_secs(3600))
            .unwrap();
        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx[0].seq, 2);
        assert_eq!(ctx[1].seq, 3);
    }

    #[test]
    fn load_context_uses_most_recent_break() {
        let (_dir, store) = make_store();
        // Two breaks — only messages after the LAST break should be returned.
        store
            .append_message(&msg("peer1", 0, "very old", MessageDirection::Outbound, 0))
            .unwrap();
        // First break at t=4000 (>1h gap)
        store
            .append_message(&msg("peer1", 1, "middle", MessageDirection::Outbound, 4000))
            .unwrap();
        store
            .append_message(&msg("peer1", 2, "middle2", MessageDirection::Inbound, 4300))
            .unwrap();
        // Second break at t=12000 (>1h gap)
        store
            .append_message(&msg("peer1", 3, "new1", MessageDirection::Outbound, 12000))
            .unwrap();
        store
            .append_message(&msg("peer1", 4, "new2", MessageDirection::Inbound, 12300))
            .unwrap();

        let ctx = store
            .load_context_after_break("peer1", Duration::from_secs(3600))
            .unwrap();
        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx[0].seq, 3);
    }

    #[test]
    fn message_count() {
        let (_dir, store) = make_store();
        assert_eq!(store.message_count("peer1").unwrap(), 0);
        store
            .append_message(&msg("peer1", 0, "hi", MessageDirection::Outbound, 0))
            .unwrap();
        store
            .append_message(&msg("peer1", 1, "ho", MessageDirection::Inbound, 10))
            .unwrap();
        assert_eq!(store.message_count("peer1").unwrap(), 2);
    }

    #[test]
    fn search_regex_finds_match() {
        let (_dir, store) = make_store();
        store
            .append_message(&msg(
                "peer1",
                0,
                "implement the auth module",
                MessageDirection::Outbound,
                0,
            ))
            .unwrap();
        store
            .append_message(&msg(
                "peer1",
                1,
                "done, tests pass",
                MessageDirection::Inbound,
                60,
            ))
            .unwrap();

        let results = store.search(None, "auth", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].seq, 0);
    }

    #[test]
    fn search_regex_case_insensitive_with_flag() {
        let (_dir, store) = make_store();
        store
            .append_message(&msg(
                "peer1",
                0,
                "AUTH module is ready",
                MessageDirection::Outbound,
                0,
            ))
            .unwrap();

        let results = store.search(None, "(?i)auth", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_regex_anchored_pattern() {
        let (_dir, store) = make_store();
        store
            .append_message(&msg(
                "peer1",
                0,
                "ERROR: disk full",
                MessageDirection::Inbound,
                0,
            ))
            .unwrap();
        store
            .append_message(&msg(
                "peer1",
                1,
                "no errors today",
                MessageDirection::Inbound,
                60,
            ))
            .unwrap();

        let results = store.search(None, "^ERROR", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_invalid_regex_returns_error() {
        let (_dir, store) = make_store();
        assert!(store.search(None, "[invalid", 10).is_err());
    }

    #[test]
    fn search_scoped_to_peer() {
        let (_dir, store) = make_store();
        store
            .append_message(&msg("peer1", 0, "auth done", MessageDirection::Outbound, 0))
            .unwrap();
        store
            .append_message(&msg(
                "peer2",
                0,
                "auth pending",
                MessageDirection::Outbound,
                0,
            ))
            .unwrap();

        let results = store.search(Some("peer1"), "auth", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].peer_id, "peer1");
    }

    #[test]
    fn list_peers_with_history() {
        let (_dir, store) = make_store();
        store
            .append_message(&msg("peer1", 0, "hi", MessageDirection::Outbound, 0))
            .unwrap();
        store
            .append_message(&msg("peer2", 0, "hello", MessageDirection::Outbound, 0))
            .unwrap();

        let peers = store.list_peers_with_history().unwrap();
        assert_eq!(peers.len(), 2);
    }

    #[test]
    fn room_append_and_read() {
        let (_dir, store) = make_store();
        let post = RoomRecord {
            message_id: Uuid::new_v4(),
            room: "firmware-team".to_string(),
            sender_peer_id: "peer1".to_string(),
            sender_name: "alice".to_string(),
            timestamp: Utc::now(),
            content: vec![ContentBlock::text("build passed")],
            depth: 0,
        };
        store.append_room_post(&post).unwrap();
        let history = store
            .read_room_history("firmware-team", None, 10, None)
            .unwrap();
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn room_history_regex_filter() {
        let (_dir, store) = make_store();
        let make = |text: &str| RoomRecord {
            message_id: Uuid::new_v4(),
            room: "dev".to_string(),
            sender_peer_id: "p".to_string(),
            sender_name: "a".to_string(),
            timestamp: Utc::now(),
            content: vec![ContentBlock::text(text)],
            depth: 0,
        };
        store.append_room_post(&make("build passed")).unwrap();
        store.append_room_post(&make("tests failed")).unwrap();

        let results = store
            .read_room_history("dev", None, 10, Some("passed"))
            .unwrap();
        assert_eq!(results.len(), 1);
    }
}
