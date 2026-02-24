// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
mod markdown;
mod queue;
pub mod conversation;
pub mod history;
pub mod frontmatter;

pub use markdown::{parse_workflow, ParsedWorkflow};
pub use queue::{Step, StepOptions, StepQueue};
pub use conversation::{
    parse_conversation, parse_jsonl_conversation, parse_jsonl_full,
    serialize_conversation, serialize_conversation_turn,
    serialize_conversation_turn_with_metadata, serialize_jsonl_conversation_turn,
    serialize_jsonl_records,
    ConversationRecord, ParsedConversation, ParsedJsonlConversation, TurnMetadata,
};
pub use frontmatter::{parse_frontmatter, WorkflowMetadata};
