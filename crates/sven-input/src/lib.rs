// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
pub mod chat_document;
pub mod conversation;
pub mod frontmatter;
pub mod history;
mod markdown;
mod queue;

pub use chat_document::{
    chat_dir, chat_path, ensure_chat_dir, json_str_to_yaml, list_chats, load_chat, load_chat_from,
    load_chat_from_with_metadata, load_chat_with_metadata, parse_chat_document, records_to_turns,
    save_chat, save_chat_atomic, save_chat_to, save_chat_to_atomic, serialize_chat_document,
    turns_to_messages, turns_to_records, yaml_to_json_str, ChatDocument, ChatEntry, ChatStatus,
    FileMetadata, FileModifiedError, SessionId, TurnRecord,
};
pub use conversation::{
    parse_conversation, parse_jsonl_conversation, parse_jsonl_full, serialize_conversation,
    serialize_conversation_turn, serialize_conversation_turn_with_metadata,
    serialize_jsonl_conversation_turn, serialize_jsonl_records, ConversationFile,
    ConversationRecord, ParsedJsonlConversation, TurnMetadata,
};
pub use frontmatter::{parse_frontmatter, WorkflowMetadata};
pub use history::{make_title, sanitize_llm_title};
pub use markdown::{parse_workflow, ParsedWorkflow};
pub use queue::{Step, StepOptions, StepQueue};
