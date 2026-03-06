// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
mod agent;
mod app;
mod chat;
mod commands;
mod input;
mod input_wrap;
mod keys;
mod layout;
mod markdown;
pub mod node_agent;
mod nvim;
mod overlay;
mod pager;
mod state;
mod submit;
mod ui;

pub use app::{App, AppOptions, ModelDirective, NodeBackend, QueuedMessage};
pub use chat::segment::ChatSegment;
pub use sven_input::history::{save as history_save, save_to as history_save_to};
pub use sven_input::serialize_jsonl_records;
pub use sven_input::ConversationRecord;
