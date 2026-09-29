// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
pub mod chat_document;
pub mod conversation;
pub mod frontmatter;
mod markdown;
mod queue;
pub mod reward;
pub mod session_resolve;
pub mod title;
pub mod trace_session;

// Legacy YAML chat support: only the read-only import surface the TUI
// actually uses is re-exported; the rest (`chat_dir`, `list_chats`,
// `parse_chat_document`, `turns_to_records`, `ChatDocument`, ...) stays
// reachable under `chat_document::` for the crate-internal legacy listing
// and the importer.
pub use chat_document::{
    chat_path, ensure_chat_dir, json_str_to_yaml, load_chat_from, yaml_to_json_str, ChatStatus,
    ChatUsage, SessionId, TurnRecord,
};
pub use conversation::{
    parse_conversation, parse_jsonl_full, serialize_conversation, serialize_conversation_turn,
    serialize_conversation_turn_with_metadata, serialize_jsonl_records, ConversationFile,
    ConversationRecord, ParsedJsonlConversation, TurnMetadata,
};
pub use frontmatter::{parse_frontmatter, WorkflowMetadata};
pub use markdown::{parse_workflow, ParsedWorkflow};
pub use queue::{Step, StepOptions, StepQueue};
pub use reward::{
    apply_outcome_to_trajectory, trajectory_reward, OutcomeFold, RunConclusion, SessionOutcome,
    SessionReward, Verdict,
};
pub use session_resolve::resolve_session_id;
pub use title::{make_title, sanitize_llm_title};
pub use trace_session::{
    chat_usage_to_final_metrics, conversation_records_to_steps,
    conversation_records_to_steps_with_copied_context, copied_context_steps, default_agent_profile,
    ensure_session_dir, final_metrics_to_chat_usage, import_legacy_chat_document,
    list_all_sessions, list_sessions, load_session_from, messages_to_steps, migrate_legacy_chats,
    new_session_id, session_path, steps_to_conversation_records, steps_to_messages,
    steps_to_turn_records, turn_records_to_steps, turn_records_to_steps_with_copied_context,
    ContextCompactionDetails, MigrationSummary, SessionEntry, StepAssembler, SvenSessionMeta,
    UnifiedSessionEntry, ATIF_SCHEMA_VERSION,
};
