// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
pub mod chat_document;
pub mod conversation;
pub mod frontmatter;
pub mod history;
mod markdown;
mod queue;
pub mod trace_session;

pub use chat_document::{
    chat_dir, chat_path, ensure_chat_dir, json_str_to_yaml, list_chats, load_chat, load_chat_from,
    parse_chat_document, turns_to_records, yaml_to_json_str, ChatDocument, ChatEntry, ChatStatus,
    ChatUsage, SessionId, TurnRecord,
};
pub use conversation::{
    parse_conversation, parse_jsonl_full, serialize_conversation, serialize_conversation_turn,
    serialize_conversation_turn_with_metadata, serialize_jsonl_records, ConversationFile, ConversationRecord,
    ParsedJsonlConversation, TurnMetadata,
};
pub use frontmatter::{parse_frontmatter, WorkflowMetadata};
pub use history::{make_title, sanitize_llm_title};
pub use markdown::{parse_workflow, ParsedWorkflow};
pub use queue::{Step, StepOptions, StepQueue};
pub use trace_session::{
    chat_usage_to_final_metrics, conversation_records_to_steps,
    conversation_records_to_steps_with_copied_context, copied_context_steps, default_agent_profile,
    ensure_session_dir, final_metrics_to_chat_usage, import_legacy_chat_document, list_all_sessions,
    list_sessions, load_session, load_session_from, load_session_with_fingerprint, messages_to_steps,
    new_session_id, record_subagent_spawn, save_session, save_session_atomic, session_dir, session_path,
    steps_to_conversation_records, steps_to_messages, steps_to_turn_records, turn_records_to_steps,
    turn_records_to_steps_with_copied_context, ContextCompactionDetails, SessionEntry, StepAssembler,
    SvenSessionMeta, UnifiedSessionEntry, ATIF_SCHEMA_VERSION,
};
