// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Re-exports the `## User`/`## Sven` markdown conversation codec from
//! [`sven_session_model`] (moved there in Phase 3.7 of the crate-architecture
//! refactor plan — it is pure parsing/formatting logic with no file-I/O
//! dependency, and `sven-ci` needs it too) and keeps the full-fidelity JSONL
//! conversation format, which is a genuinely different concern (an
//! adjacently-tagged, append-only line format capturing thinking blocks and
//! compaction markers the markdown codec has no representation for) that
//! stayed here.

use serde::{Deserialize, Serialize};
use sven_model::{Message, Role};
pub use sven_session_model::{
    parse_conversation, serialize_conversation, serialize_conversation_turn,
    serialize_conversation_turn_with_metadata, ConversationFile, ParseError, TurnMetadata,
};

// ── Full-fidelity JSONL record format ─────────────────────────────────────────

/// A single record in a full-fidelity JSONL conversation file.
///
/// Unlike the old raw-`Message` JSONL format this type captures every element
/// that can appear in a conversation, including thinking/reasoning traces and
/// context-compaction notes, making it possible to restore a session exactly.
///
/// Wire format (adjacently tagged, one record per line):
/// - Message:          `{"type":"message","data":{<Message fields>}}`
/// - Thinking:         `{"type":"thinking","data":{"content":"..."}}`
/// - ContextCompacted: `{"type":"context_compacted","data":{"tokens_before":N,"tokens_after":M,...}}`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ConversationRecord {
    /// Any conversation message (system / user / assistant / tool).
    ///
    /// System messages are stored for completeness; callers must strip them
    /// before seeding the agent (the agent always regenerates a fresh system
    /// message at runtime from the current config and tools).
    Message(Message),
    /// A reasoning / thinking block produced by the model during a turn.
    Thinking { content: String },
    /// A marker left when the session history was compacted to save context.
    ContextCompacted {
        tokens_before: usize,
        tokens_after: usize,
        /// Which compaction strategy was used (structured/narrative/emergency).
        /// Optional for backward compatibility with older JSONL files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strategy: Option<String>,
        /// Agentic loop round in which compaction fired (0 = pre-submit).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u32>,
    },
}

/// Result of parsing a full-fidelity JSONL conversation file.
pub struct ParsedJsonlConversation {
    /// All records in file order (messages, thinking blocks, compaction notes).
    pub records: Vec<ConversationRecord>,
    /// Non-system messages only, with the pending user turn stripped.
    /// Pass to `agent.seed_history()` when resuming.
    pub history: Vec<Message>,
    /// The text of the last user message if it has no following assistant
    /// response.  `None` when there is nothing pending.
    pub pending_user_input: Option<String>,
    /// The text content of the system message found in the file, if any.
    /// When present and `--regen-system-prompt` is not set, callers should
    /// use this as the `system_prompt_override` so that resumed conversations
    /// use the exact same prompt they were started with.
    pub system_message: Option<String>,
}

/// Parse a full-fidelity JSONL conversation file.
///
/// Handles both the new `ConversationRecord` tagged format and the legacy raw
/// `Message` format so that files written by older versions of sven still load.
pub fn parse_jsonl_full(content: &str) -> Result<ParsedJsonlConversation, ParseError> {
    let mut records: Vec<ConversationRecord> = Vec::new();

    for (line_no, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| ParseError::InvalidJsonlLine {
                line: line_no + 1,
                error: e.to_string(),
            })?;

        if v.get("type").is_some() {
            // New tagged format.
            let record: ConversationRecord =
                serde_json::from_value(v).map_err(|e| ParseError::InvalidJsonlLine {
                    line: line_no + 1,
                    error: e.to_string(),
                })?;
            records.push(record);
        } else {
            // Legacy format - raw Message JSON.
            let msg: Message =
                serde_json::from_value(v).map_err(|e| ParseError::InvalidJsonlLine {
                    line: line_no + 1,
                    error: e.to_string(),
                })?;
            records.push(ConversationRecord::Message(msg));
        }
    }

    // Extract the first system message text (if any) for the caller to reuse.
    let system_message: Option<String> = records.iter().find_map(|r| {
        if let ConversationRecord::Message(m) = r {
            if m.role == Role::System {
                return m.as_text().map(|t| t.to_string());
            }
        }
        None
    });

    // Build history: only Message records, system messages stripped, pending stripped.
    let messages: Vec<Message> = records
        .iter()
        .filter_map(|r| {
            if let ConversationRecord::Message(m) = r {
                if m.role != Role::System {
                    return Some(m.clone());
                }
            }
            None
        })
        .collect();

    let (history, pending_user_input) = match messages.last() {
        Some(m) if m.role == Role::User => {
            let pending = m.as_text().unwrap_or("").to_string();
            let history = messages[..messages.len() - 1].to_vec();
            (history, Some(pending))
        }
        _ => (messages, None),
    };

    Ok(ParsedJsonlConversation {
        records,
        history,
        pending_user_input,
        system_message,
    })
}

/// Serialize a slice of `ConversationRecord`s to JSONL.
///
/// Each record becomes one line.  Suitable for both full-file writes and
/// append-only updates.
pub fn serialize_jsonl_records(records: &[ConversationRecord]) -> String {
    let mut result = String::new();
    for record in records {
        match serde_json::to_string(record) {
            Ok(line) => {
                result.push_str(&line);
                result.push('\n');
            }
            Err(e) => {
                tracing::warn!("failed to serialize ConversationRecord to JSONL: {e}");
            }
        }
    }
    result
}
