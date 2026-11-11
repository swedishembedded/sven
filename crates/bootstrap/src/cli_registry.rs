// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The tool registry `sven tool` lists and runs from: every built-in tool,
//! with no live model and no session behind it.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use crate::registry::register_session_tools;
use crate::Config;
use sven_tool_api::events::{TodoItem, ToolEvent};
use sven_tool_registry::ToolRegistry;
use sven_tools_agent::SkillTool;
#[cfg(feature = "document")]
use sven_tools_document::{DocumentConvertTool, DocumentLocateTool};
#[cfg(feature = "media")]
use sven_tools_fs::AttachFileTool;
use sven_tools_fs::{EditFileTool, FindFileTool, ReadFileTool, WriteTool};
#[cfg(all(unix, feature = "gdb"))]
use sven_tools_gdb::GdbSessionState;
use sven_tools_web::GrepTool;
use sven_workspace::Shared;

#[cfg(all(unix, feature = "gdb"))]
use crate::GdbTool;

/// Build a lightweight [`ToolRegistry`] for direct CLI invocation.
///
/// Contains the same consolidated tool set as the agent, minus tools that
/// require a live model or TUI channel. Intended for `sven tool <name> <args>`
/// direct invocation.
pub fn build_cli_tool_registry(cfg: &Config) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    // ── File I/O ─────────────────────────────────────────────────────────────
    reg.register(ReadFileTool::default());
    reg.register(FindFileTool::default());
    reg.register(WriteTool::default());
    reg.register_with_display(EditFileTool::default());

    // ── PDF to Markdown ───────────────────────────────────────────────────────
    #[cfg(feature = "document")]
    reg.register_with_display(DocumentLocateTool::new(
        sven_tools_document::DocumentAccess::local(sven_tool_api::PathScope::unconfined()),
    ));
    #[cfg(feature = "document")]
    reg.register_with_display(DocumentConvertTool::new(
        sven_tools_document::DocumentAccess::local(sven_tool_api::PathScope::unconfined()),
        Arc::new(crate::ConfigOcrModels::new(cfg.clone())),
        crate::convert_options(&cfg.ocr),
    ));

    // ── Multimodal attachments ───────────────────────────────────────────────
    // No live model in the CLI registry, so audio is always transcribed —
    // the only answer that is correct for every possible target model.
    #[cfg(feature = "media")]
    reg.register(AttachFileTool::new(None, cfg.tools.asr.clone()));

    // ── Search ────────────────────────────────────────────────────────────────
    reg.register(GrepTool::default());

    // ── Web ───────────────────────────────────────────────────────────────────
    reg.register(cfg.tools.web.fetch_tool());
    reg.register(cfg.web_search_tool());

    // ── System ────────────────────────────────────────────────────────────────
    reg.register(cfg.tools.shell_tool(sven_tool_api::PathScope::default()));

    let (event_tx, _event_rx) = mpsc::channel::<ToolEvent>(16);
    let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
    register_session_tools(&mut reg, todos, event_tx.clone());

    reg.register(SkillTool::new(Shared::empty()));

    // ── Memory ────────────────────────────────────────────────────────────────
    let knowledge = Shared::empty();
    reg.register(cfg.tools.memory.tool(knowledge));

    // ── Context (no model available for query/reduce) ─────────────────────────
    // Only open/read/grep are fully usable without a model.
    // The compound context tool is not registered in CLI mode since query/reduce
    // require a live model provider.

    // ── GDB ───────────────────────────────────────────────────────────────────
    // GDB tools use Unix signal APIs and are only available on Unix platforms.
    #[cfg(all(unix, feature = "gdb"))]
    {
        let gdb_state = Arc::new(Mutex::new(GdbSessionState::default()));
        reg.register(
            GdbTool::new(gdb_state, cfg.tools.gdb.clone())
                .with_command_policy(Arc::new(cfg.tools.policy())),
        );
    }

    reg
}
