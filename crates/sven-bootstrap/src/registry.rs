//! Centralised tool-registry builder.
//!
//! All callers (CI runner, conversation runner, TUI, sub-agents) use
//! `build_tool_registry` with the appropriate [`ToolSetProfile`] instead of
//! each inlining their own registration loop.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_config::{AgentMode, Config};
use sven_model::ModelProvider;
use sven_tools::{
    AskQuestionTool, ApplyPatchTool, DeleteFileTool, EditFileTool,
    FsTool, GlobFileSearchTool, GlobTool, GrepTool, ListDirTool,
    ReadFileTool, ReadImageTool, ReadLintsTool, RunTerminalCommandTool, SearchCodebaseTool,
    ShellTool, SwitchModeTool, TodoWriteTool, UpdateMemoryTool,
    WebFetchTool, WebSearchTool, WriteTool,
    GdbStartServerTool, GdbConnectTool, GdbCommandTool,
    GdbInterruptTool, GdbWaitStoppedTool, GdbStatusTool, GdbStopTool,
    GdbSessionState, ToolRegistry,
    events::ToolEvent,
};

use sven_core::AgentRuntimeContext;

use crate::context::ToolSetProfile;
use crate::task_tool::TaskTool;

/// Build a [`ToolRegistry`] populated according to the given `profile`.
///
/// This is the single canonical place where tools are wired up.  Adding a
/// new tool to sven means adding it here once and it will appear in every
/// appropriate profile automatically.
///
/// ### Shared-state parameters
///
/// * `mode_lock` — the **same** `Arc` that will be passed to `Agent::new()`.
///   `SwitchModeTool` holds a clone so that mode changes are immediately
///   visible to the agent loop via `drain_tool_events`.
/// * `tool_event_tx` — the sending half of the channel whose receiving end
///   is passed to `Agent::new()`.  `TodoWriteTool` and `SwitchModeTool` send
///   events here; the agent drains them after each tool execution.
/// * `sub_agent_runtime` — inherited by `TaskTool` sub-agents (project root,
///   CI/git notes, AGENTS.md).  Only used for the `Full` profile; pass
///   `AgentRuntimeContext::default()` otherwise.
pub fn build_tool_registry(
    cfg: &Config,
    model: Arc<dyn ModelProvider>,
    profile: ToolSetProfile,
    mode_lock: Arc<Mutex<AgentMode>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    sub_agent_runtime: AgentRuntimeContext,
) -> ToolRegistry {
    match profile {
        ToolSetProfile::Full { question_tx, todos, task_depth } => {
            let mut reg = ToolRegistry::new();

            reg.register(ReadFileTool);
            reg.register(ReadImageTool);
            reg.register(ListDirTool);
            reg.register(FsTool);
            reg.register(GlobFileSearchTool);
            reg.register(GlobTool);
            reg.register(GrepTool);
            reg.register(SearchCodebaseTool);
            reg.register(WebFetchTool);
            reg.register(WebSearchTool {
                api_key: cfg.tools.web.search.api_key.clone(),
            });
            reg.register(ReadLintsTool);
            reg.register(UpdateMemoryTool {
                memory_file: cfg.tools.memory.memory_file.clone(),
            });
            let ask_tool = match question_tx {
                Some(tx) => AskQuestionTool::new_tui(tx),
                None => AskQuestionTool::new(),
            };
            reg.register(ask_tool);
            reg.register(TodoWriteTool::new(todos, tool_event_tx.clone()));
            reg.register(SwitchModeTool::new(mode_lock, tool_event_tx));
            reg.register(WriteTool);
            reg.register(EditFileTool);
            reg.register(DeleteFileTool);
            reg.register(ApplyPatchTool);
            reg.register(RunTerminalCommandTool {
                timeout_secs: cfg.tools.timeout_secs,
            });
            reg.register(ShellTool { timeout_secs: cfg.tools.timeout_secs });
            reg.register(TaskTool::new(
                model,
                Arc::new(cfg.clone()),
                task_depth,
                sub_agent_runtime,
            ));

            let gdb_state = Arc::new(Mutex::new(GdbSessionState::default()));
            reg.register(GdbStartServerTool::new(gdb_state.clone(), cfg.tools.gdb.clone()));
            reg.register(GdbConnectTool::new(gdb_state.clone(), cfg.tools.gdb.clone()));
            reg.register(GdbCommandTool::new(gdb_state.clone(), cfg.tools.gdb.clone()));
            reg.register(GdbInterruptTool::new(gdb_state.clone()));
            reg.register(GdbWaitStoppedTool::new(gdb_state.clone()));
            reg.register(GdbStatusTool::new(gdb_state.clone()));
            reg.register(GdbStopTool::new(gdb_state));

            reg
        }

        ToolSetProfile::SubAgent { todos } => {
            let mut reg = ToolRegistry::new();

            reg.register(ReadFileTool);
            reg.register(ReadImageTool);
            reg.register(ListDirTool);
            reg.register(FsTool);
            reg.register(GlobFileSearchTool);
            reg.register(GlobTool);
            reg.register(GrepTool);
            reg.register(SearchCodebaseTool);
            reg.register(WebFetchTool);
            reg.register(WebSearchTool {
                api_key: cfg.tools.web.search.api_key.clone(),
            });
            reg.register(ReadLintsTool);
            reg.register(UpdateMemoryTool {
                memory_file: cfg.tools.memory.memory_file.clone(),
            });
            reg.register(AskQuestionTool::new());
            reg.register(TodoWriteTool::new(todos, tool_event_tx.clone()));
            reg.register(SwitchModeTool::new(mode_lock, tool_event_tx));
            reg.register(WriteTool);
            reg.register(EditFileTool);
            reg.register(DeleteFileTool);
            reg.register(ApplyPatchTool);
            reg.register(RunTerminalCommandTool {
                timeout_secs: cfg.tools.timeout_secs,
            });
            reg.register(ShellTool { timeout_secs: cfg.tools.timeout_secs });
            // TaskTool intentionally omitted to limit sub-agent nesting

            let gdb_state = Arc::new(Mutex::new(GdbSessionState::default()));
            reg.register(GdbStartServerTool::new(gdb_state.clone(), cfg.tools.gdb.clone()));
            reg.register(GdbConnectTool::new(gdb_state.clone(), cfg.tools.gdb.clone()));
            reg.register(GdbCommandTool::new(gdb_state.clone(), cfg.tools.gdb.clone()));
            reg.register(GdbInterruptTool::new(gdb_state.clone()));
            reg.register(GdbWaitStoppedTool::new(gdb_state.clone()));
            reg.register(GdbStatusTool::new(gdb_state.clone()));
            reg.register(GdbStopTool::new(gdb_state));

            reg
        }
    }
}
