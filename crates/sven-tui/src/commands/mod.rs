// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Slash command system — re-exported from `sven-frontend`.

pub use sven_frontend::commands::completion;
pub use sven_frontend::commands::mcp;
pub use sven_frontend::commands::parser;
pub use sven_frontend::commands::{
    dispatch_command, parse, CommandContext, CommandRegistry, CompletionItem, CompletionManager,
    ImmediateAction, ParsedCommand, SlashCommand,
};
