// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Centralised tool-registry builder.
//!
//! All callers (CI runner, conversation runner, TUI, sub-agents) use
//! `build_tool_registry` with the appropriate [`ToolSetProfile`] instead of
//! each inlining their own registration loop.
//!
//! ## Tool consolidation
//!
//! The registry exposes a small set of compound tools (e.g. one `gdb` tool
//! with an action parameter) rather than one tool per operation. This
//! reduces the model's decision surface, cuts input token cost, and keeps
//! the Anthropic prefix-cache stable across turns.
//!
//! Individual tools (`gdb_start_server`, `buf_read`, etc.) remain Rust types
//! for testing and internal use but are not registered as separate entries
//! in the tool registry.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_config::{AgentMode, Config};
use sven_model::ModelProvider;
use sven_tool_api::events::{TodoItem, ToolEvent};
use sven_tool_registry::ToolRegistry;
use sven_tools_agent::{AskQuestionTool, ModelCatalogEntry, SkillTool, SystemTool, TodoTool};
use sven_tools_ctx::{ContextStore, MemoryTool};
use sven_tools_exec::ShellTool;
#[cfg(feature = "media")]
use sven_tools_fs::AttachFileTool;
use sven_tools_fs::{EditFileTool, FindFileTool, OutputBufferStore, ReadFileTool, WriteTool};
#[cfg(all(unix, feature = "gdb"))]
use sven_tools_gdb::GdbSessionState;
use sven_tools_web::{GrepTool, WebFetchTool, WebSearchTool};
use sven_workspace::Shared;

use sven_turn::AgentRuntimeContext;

use crate::context::{Questions, ToolSetProfile};
use crate::context_tool::ContextTool;
use crate::task_tool::{ChildApprover, TaskTool};
#[cfg(all(unix, feature = "gdb"))]
use crate::GdbTool;

// ── Integration tool providers ────────────────────────────────────────────────

/// Optional services the host provides to tools.
///
/// All fields are optional; a tool that needs one is registered only when it
/// is set, and a tool that can use one works without it.
#[derive(Default)]
pub struct IntegrationProviders {
    /// Semantic memory store for the `semantic_memory` tool.
    #[cfg(feature = "memory")]
    pub memory_store: Option<Arc<dyn sven_memory::VectorStore>>,
    /// Who answers a `task` sub-agent's permission requests that the
    /// session's policy does not allow outright: the host's requester, or the
    /// session's own approval gate. Without one they are refused.
    pub approver: Option<ChildApprover>,
    /// The session's approval mode, which its `task` sub-agents are held to.
    pub approval_mode: sven_config::ApprovalMode,
}

/// The semantic memory store (SQLite + FTS5) for the `semantic_memory` tool.
///
/// `open` fails only on a broken `$HOME`/on-disk state (permissions,
/// corruption). That is not fatal to the session: the tool is left
/// unregistered and the reason logged, exactly like a missing MCP tool.
#[cfg(feature = "memory")]
pub(crate) async fn open_memory_store() -> Option<Arc<dyn sven_memory::VectorStore>> {
    match sven_memory::SqliteMemoryStore::open(None).await {
        Ok(store) => Some(Arc::new(store)),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "failed to open semantic memory store; semantic_memory tool will not be registered"
            );
            None
        }
    }
}

/// Converts the model catalog into the slice-of-fields `SystemTool`'s
/// `switch_model` fuzzy search needs, without giving `sven-tools-agent` a direct
/// dependency on `sven-model` for the sake of one tool's lookup.
fn model_catalog_for_tools() -> Vec<ModelCatalogEntry> {
    sven_model::catalog::static_catalog()
        .into_iter()
        .map(|e| ModelCatalogEntry {
            id: e.id,
            name: e.name,
            provider: e.provider,
        })
        .collect()
}

/// Build a [`ToolRegistry`] populated according to the given `profile`.
///
/// This is the single canonical place where tools are wired up.
///
/// ### Shared-state parameters
///
/// * `mode_lock` - shared with the kernel; `SystemTool` holds a clone so
///   that mode changes are immediately visible to the running turn.
/// * `tool_event_tx` - the sending half of the channel whose receiving end is
///   drained by the kernel's tool executor. `TodoTool` / `SystemTool` send
///   events here.
///
/// The `buffer_store` is bundled inside the `profile` variants that need it
/// (`Full`, `Coding`, `SubAgent`).
///
/// Pass `integrations` to register the memory tools. All fields are
/// optional; only providers that are `Some` get registered.
pub fn build_tool_registry(
    cfg: &Config,
    model: Arc<dyn ModelProvider>,
    profile: ToolSetProfile,
    mode_lock: Arc<Mutex<AgentMode>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    sub_agent_runtime: AgentRuntimeContext,
) -> ToolRegistry {
    build_tool_registry_with_integrations(
        cfg,
        model,
        profile,
        mode_lock,
        tool_event_tx,
        sub_agent_runtime,
        IntegrationProviders::default(),
    )
}

/// Build a [`ToolRegistry`] with optional integration tool providers.
///
/// This is the extended version of [`build_tool_registry`] that also registers
/// the memory integration tools when providers are supplied.
pub fn build_tool_registry_with_integrations(
    cfg: &Config,
    model: Arc<dyn ModelProvider>,
    profile: ToolSetProfile,
    mode_lock: Arc<Mutex<AgentMode>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    sub_agent_runtime: AgentRuntimeContext,
    integrations: IntegrationProviders,
) -> ToolRegistry {
    let mut reg = match profile {
        ToolSetProfile::Full {
            questions,
            todos,
            buffer_store,
        } => build_profile_full(FullProfileParams {
            cfg,
            model,
            mode_lock,
            questions,
            todos,
            tool_event_tx,
            runtime: &sub_agent_runtime,
            buffer_store,
            include_gdb_context: true,
            approver: integrations.approver.clone(),
            approval_mode: integrations.approval_mode,
        }),
        ToolSetProfile::Coding {
            questions,
            todos,
            buffer_store,
        } => build_profile_full(FullProfileParams {
            cfg,
            model,
            mode_lock,
            questions,
            todos,
            tool_event_tx,
            runtime: &sub_agent_runtime,
            buffer_store,
            include_gdb_context: false,
            approver: integrations.approver.clone(),
            approval_mode: integrations.approval_mode,
        }),
        ToolSetProfile::Research { questions, todos } => build_profile_research(
            cfg,
            mode_lock,
            questions,
            todos,
            tool_event_tx,
            &sub_agent_runtime,
            &integrations,
        ),
        ToolSetProfile::SubAgent {
            todos,
            buffer_store,
        } => build_profile_subagent(
            cfg,
            model,
            mode_lock,
            todos,
            tool_event_tx,
            &sub_agent_runtime,
            buffer_store,
        ),
    };

    // Register integration tools if providers are available.
    register_integration_tools(&mut reg, integrations);

    reg
}

/// Register integration tools into an existing registry based on available providers.
fn register_integration_tools(_reg: &mut ToolRegistry, _providers: IntegrationProviders) {
    // `memory` is a feature (see this crate's Cargo.toml) so a `minimal`
    // build can exclude `rusqlite` while every other build keeps semantic
    // memory on by default.
    #[cfg(feature = "memory")]
    {
        if let Some(store) = _providers.memory_store {
            _reg.register(sven_memory::SemanticMemoryTool::new(store));
        }
    }
}

/// The `system` tool for a session that starts in `mode_lock`'s mode: it may
/// switch only to modes within that mode's authority, and manages MCP
/// servers only when `manages_mcp` (never in a read-only or sub-agent
/// session).
fn system_tool(
    mode_lock: Arc<Mutex<AgentMode>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    manages_mcp: bool,
) -> SystemTool {
    // The lock was just created with the session's starting mode, so nobody
    // else holds it. Should someone, the start is unknown and no switch is
    // allowed: any mode assumed in its place could be wider.
    let allowed = mode_lock.try_lock().map_or_else(
        |_| Vec::new(),
        |start| crate::mode_policy::modes_within(*start),
    );
    let tool = SystemTool::new(mode_lock, tool_event_tx, model_catalog_for_tools())
        .with_allowed_modes(allowed);
    if manages_mcp {
        tool
    } else {
        tool.without_mcp_management()
    }
}

/// Offers `ask_question` routed as `questions` says, if at all.
pub(crate) fn register_ask_question(reg: &mut ToolRegistry, questions: Questions) {
    match questions {
        Questions::Answered(tx) => reg.register(AskQuestionTool::new_tui(tx)),
        Questions::Parked => reg.register(AskQuestionTool::parking()),
        Questions::NoUser => reg.register(AskQuestionTool::no_user()),
        Questions::Unavailable => {}
    }
}

/// Parameters shared by the Full and Coding profile builders.
struct FullProfileParams<'a> {
    cfg: &'a Config,
    model: Arc<dyn ModelProvider>,
    mode_lock: Arc<Mutex<AgentMode>>,
    questions: Questions,
    todos: Arc<Mutex<Vec<TodoItem>>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &'a AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    include_gdb_context: bool,
    approver: Option<ChildApprover>,
    approval_mode: sven_config::ApprovalMode,
}

/// Full and Coding profiles share the same builder; `include_gdb_context`
/// controls whether GDB and context tools are included.
fn build_profile_full(p: FullProfileParams<'_>) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    // What sub-agents are told to run on: resolvable by a child process
    // loading the same config (a named provider stays named).
    let model_id = p.cfg.model_reference();
    let parent_mode = Arc::clone(&p.mode_lock);
    reg.register(system_tool(
        Arc::clone(&p.mode_lock),
        p.tool_event_tx.clone(),
        true,
    ));

    register_base_tools(
        &mut reg,
        p.cfg,
        p.model,
        p.tool_event_tx.clone(),
        p.runtime,
        Arc::clone(&p.buffer_store),
        p.include_gdb_context,
    );

    register_ask_question(&mut reg, p.questions);
    reg.register(TodoTool::new(p.todos, p.tool_event_tx.clone()));

    reg.register(
        TaskTool::new(
            Arc::clone(&p.buffer_store),
            p.tool_event_tx,
            Some(model_id),
            p.runtime.agents.clone(),
            parent_mode,
        )
        .with_scope(p.runtime.path_scope.clone())
        .with_wall_clock(p.cfg.agent.child_run_timeout())
        .with_turn_budgets(
            Some(p.cfg.agent.max_tool_rounds),
            p.cfg.model.max_output_tokens,
        )
        .with_approver(p.approver, p.approval_mode)
        .with_disabled_tools(p.cfg.tools.disabled.clone())
        .with_command_patterns(&p.cfg.tools),
    );

    reg
}

/// Research profile: read-only tools; `task` may delegate only to read-only
/// children (the tool enforces the ceiling against the live mode).
fn build_profile_research(
    cfg: &Config,
    mode_lock: Arc<Mutex<AgentMode>>,
    questions: Questions,
    todos: Arc<Mutex<Vec<TodoItem>>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &AgentRuntimeContext,
    integrations: &IntegrationProviders,
) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    // Read-only file tools only.
    let paths = &runtime.path_scope;
    reg.register(ReadFileTool::new(paths.clone()));
    reg.register(FindFileTool::new(paths.clone()));
    reg.register(GrepTool::new(paths.clone()));
    reg.register(WebFetchTool::new(cfg.tools.web.fetch_max_chars));
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });
    reg.register(MemoryTool::new(
        cfg.tools.memory.memory_file.clone(),
        runtime.knowledge.clone(),
    ));
    reg.register(SkillTool::new(runtime.skills.clone()));
    reg.register(system_tool(
        Arc::clone(&mode_lock),
        tool_event_tx.clone(),
        false,
    ));

    register_ask_question(&mut reg, questions);
    reg.register(TodoTool::new(todos, tool_event_tx.clone()));

    // Task is included for delegation; its children are held to read-only
    // modes for as long as this session is.
    let buffer_store = Arc::new(Mutex::new(OutputBufferStore::new()));
    reg.register(
        TaskTool::new(
            buffer_store,
            tool_event_tx,
            Some(cfg.model_reference()),
            runtime.agents.clone(),
            mode_lock,
        )
        .with_scope(paths.clone())
        .with_wall_clock(cfg.agent.child_run_timeout())
        .with_turn_budgets(Some(cfg.agent.max_tool_rounds), cfg.model.max_output_tokens)
        .with_approver(integrations.approver.clone(), integrations.approval_mode)
        .with_disabled_tools(cfg.tools.disabled.clone())
        .with_command_patterns(&cfg.tools),
    );

    reg
}

/// SubAgent profile: Coding minus ask_question minus task.
fn build_profile_subagent(
    cfg: &Config,
    model: Arc<dyn ModelProvider>,
    mode_lock: Arc<Mutex<AgentMode>>,
    todos: Arc<Mutex<Vec<TodoItem>>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(system_tool(
        Arc::clone(&mode_lock),
        tool_event_tx.clone(),
        false,
    ));

    register_base_tools(
        &mut reg,
        cfg,
        model,
        tool_event_tx.clone(),
        runtime,
        buffer_store,
        false, // No GDB/context in sub-agents
    );

    // ask_question omitted: sub-agents run headless.
    // TaskTool omitted: prevent unbounded nesting.
    reg.register(TodoTool::new(todos, tool_event_tx));

    reg
}

/// Register the lean consolidated tool set shared by agent profiles.
///
/// `include_full` controls whether the GDB and context tools are included.
/// SubAgent uses the slimmer set (no GDB, no context) since sub-agents
/// typically perform focused coding/research tasks.
#[allow(clippy::too_many_arguments)]
fn register_base_tools(
    reg: &mut ToolRegistry,
    cfg: &Config,
    model: Arc<dyn ModelProvider>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    include_full: bool,
) {
    // ── File I/O ─────────────────────────────────────────────────────────────
    // read_file already handles images (auto-detected by extension). Every
    // path-taking tool resolves through the session's scope.
    let paths = &runtime.path_scope;
    reg.register(ReadFileTool::new(paths.clone()));
    reg.register(FindFileTool::new(paths.clone()));
    reg.register(WriteTool::new(paths.clone()));
    reg.register_with_display(EditFileTool::new(paths.clone()));

    // ── Multimodal attachments ───────────────────────────────────────────────
    // attach_file needs the live model to decide whether audio can be sent
    // natively or must be transcribed, so clone the Arc before `model` is
    // moved into the context tool below. Only in a build that decodes media.
    #[cfg(feature = "media")]
    reg.register(
        AttachFileTool::new(Some(Arc::clone(&model)), cfg.tools.asr.clone())
            .with_scope(paths.clone()),
    );

    // ── Search ────────────────────────────────────────────────────────────────
    // grep now supports whole_project=true (replaces search_codebase).
    reg.register(GrepTool::new(paths.clone()));

    // ── Shell ─────────────────────────────────────────────────────────────────
    // shell covers: run commands, delete files, list dirs, run linters. The
    // scope sets where a command starts, not what it may touch.
    reg.register(ShellTool {
        timeout_secs: cfg.tools.timeout_secs,
        scope: paths.clone(),
        policy: Arc::new(sven_tool_registry::ToolPolicy::from_config(&cfg.tools)),
    });

    // ── Web ───────────────────────────────────────────────────────────────────
    reg.register(WebFetchTool::new(cfg.tools.web.fetch_max_chars));
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });

    // ── Memory (KV + project knowledge) ──────────────────────────────────────
    // Compound tool: set|get|delete|list|search_knowledge|list_knowledge
    reg.register(MemoryTool::new(
        cfg.tools.memory.memory_file.clone(),
        runtime.knowledge.clone(),
    ));

    // ── Skills ────────────────────────────────────────────────────────────────
    reg.register(SkillTool::new(runtime.skills.clone()));

    // ── Context and GDB (Full profile only) ──────────────────────────────────
    if include_full {
        // Compound context tool: open|read|grep|query|reduce
        let context_store = Arc::new(Mutex::new(ContextStore::new()));
        reg.register(ContextTool::new(
            context_store,
            model,
            cfg,
            Some(tool_event_tx),
        ));

        // Compound GDB tool: start_server|connect|command|interrupt|wait_stopped|status|stop
        // GDB tools use Unix signal APIs and are only available on Unix platforms.
        #[cfg(all(unix, feature = "gdb"))]
        {
            let gdb_state = Arc::new(Mutex::new(GdbSessionState::default()));
            reg.register(
                GdbTool::new(gdb_state, cfg.tools.gdb.clone()).with_command_policy(Arc::new(
                    sven_tool_registry::ToolPolicy::from_config(&cfg.tools),
                )),
            );
        }
    } else {
        // Suppress unused warnings for the buffer_store in SubAgent path.
        let _ = buffer_store;
        let _ = tool_event_tx;
    }
}

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

    // ── Multimodal attachments ───────────────────────────────────────────────
    // No live model in the CLI registry, so audio is always transcribed —
    // the only answer that is correct for every possible target model.
    #[cfg(feature = "media")]
    reg.register(AttachFileTool::new(None, cfg.tools.asr.clone()));

    // ── Search ────────────────────────────────────────────────────────────────
    reg.register(GrepTool::default());

    // ── Web ───────────────────────────────────────────────────────────────────
    reg.register(WebFetchTool::new(cfg.tools.web.fetch_max_chars));
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });

    // ── System ────────────────────────────────────────────────────────────────
    reg.register(ShellTool {
        timeout_secs: cfg.tools.timeout_secs,
        policy: Arc::new(sven_tool_registry::ToolPolicy::from_config(&cfg.tools)),
        ..ShellTool::default()
    });

    let (event_tx, _event_rx) = mpsc::channel::<ToolEvent>(16);
    let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
    reg.register(TodoTool::new(todos, event_tx.clone()));

    reg.register(SkillTool::new(Shared::empty()));

    // ── Memory ────────────────────────────────────────────────────────────────
    let knowledge = Shared::empty();
    reg.register(MemoryTool::new(
        cfg.tools.memory.memory_file.clone(),
        knowledge,
    ));

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
            GdbTool::new(gdb_state, cfg.tools.gdb.clone()).with_command_policy(Arc::new(
                sven_tool_registry::ToolPolicy::from_config(&cfg.tools),
            )),
        );
    }

    reg
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model_mock::MockProvider;

    /// The `system` tool reads the session's starting mode to bound its mode
    /// switches; if it cannot, it allows none rather than guessing a mode.
    #[tokio::test]
    async fn a_system_tool_that_cannot_read_the_start_mode_switches_to_nothing() {
        use sven_tool_api::Tool as _;
        let (tx, _rx) = mpsc::channel::<ToolEvent>(16);
        let mode_lock = Arc::new(Mutex::new(AgentMode::Research));
        let held = Arc::clone(&mode_lock);
        let guard = held.lock().await;
        let tool = system_tool(Arc::clone(&mode_lock), tx, false);
        drop(guard);
        for mode in ["agent", "research"] {
            let out = tool
                .execute(&sven_tool_api::ToolCall {
                    id: "c".into(),
                    name: "system".into(),
                    args: serde_json::json!({"action": "switch_mode", "mode": mode}),
                })
                .await;
            assert!(out.is_error, "{mode}: {}", out.content);
        }
        assert_eq!(*mode_lock.lock().await, AgentMode::Research);
    }

    /// Every tool the agent is offered, with its description.
    fn agent_tools() -> Vec<(String, String, serde_json::Value)> {
        let (tx, _rx) = mpsc::channel::<ToolEvent>(16);
        let reg = build_tool_registry(
            &Config::default(),
            Arc::new(MockProvider),
            ToolSetProfile::Full {
                questions: Questions::Unavailable,
                todos: Arc::new(Mutex::new(Vec::new())),
                buffer_store: Arc::new(Mutex::new(OutputBufferStore::default())),
            },
            Arc::new(Mutex::new(AgentMode::Agent)),
            tx,
            AgentRuntimeContext::default(),
        );
        reg.schemas()
            .into_iter()
            .map(|s| (s.name, s.description, s.parameters))
            .collect()
    }

    /// The words a tool's own schema already defines: its parameter names and
    /// every value of an enum it accepts.
    ///
    /// A compound tool naming its own actions is describing itself, which is
    /// the opposite of the coupling under test - `context` documents an
    /// `action: grep` it implements, and that is not a reference to the `grep`
    /// tool. Taken from the schema rather than a hand-kept list, so a tool that
    /// renames an action cannot leave a stale exemption behind.
    fn own_vocabulary(parameters: &serde_json::Value) -> Vec<String> {
        let mut words = Vec::new();
        let Some(props) = parameters.get("properties").and_then(|p| p.as_object()) else {
            return words;
        };
        for (name, spec) in props {
            words.push(name.clone());
            let variants = spec
                .get("enum")
                .and_then(|e| e.as_array())
                .into_iter()
                .flatten();
            words.extend(variants.filter_map(|v| v.as_str()).map(str::to_string));
        }
        words
    }

    /// The exact tool set the agent is offered.
    ///
    /// Pinned deliberately. Every tool here is schema, description and name in
    /// the system prompt of every single request, so the set growing is a cost
    /// paid forever by every turn - and one nobody notices, because adding a
    /// tool is a one-line registration. Changing this list should be a
    /// decision, which means it has to be visible.
    #[test]
    fn the_agent_tool_set_is_what_we_think_it_is() {
        let mut got: Vec<String> = agent_tools().into_iter().map(|(n, ..)| n).collect();
        got.sort();
        let mut expected = vec![
            "context",
            "edit_file",
            "find_file",
            "grep",
            "memory",
            "read_file",
            "shell",
            "skill",
            "system",
            "task",
            "todo",
            "web_fetch",
            "web_search",
            "write_file",
        ];
        // Compiled-in tools are offered; compiled-out ones are not.
        if cfg!(feature = "media") {
            expected.push("attach_file");
        }
        if cfg!(all(unix, feature = "gdb")) {
            expected.push("gdb");
        }
        expected.sort_unstable();
        assert_eq!(got, expected, "the agent's tool set changed");
    }

    /// Every tool a machine allow-lists for a state must be one the agent is
    /// actually offered.
    ///
    /// `schemas_for_names` silently skips unknown names, so a stale list
    /// fails quietly: the SDLC execution phase listed `delete_file`, `glob`
    /// and `run_terminal_command` (none exists) and omitted `write_file`, so
    /// it - and the task and verified-task machines sharing the list - could
    /// edit files but never create one.
    #[test]
    fn every_state_scoped_tool_is_a_registered_tool() {
        use sven_machines::machines::sdlc::prompts::{BUILD_TOOLS, READ_TOOLS, WRITE_TOOLS};
        let known: Vec<String> = agent_tools().into_iter().map(|(n, ..)| n).collect();
        let mut unknown: Vec<(&str, &str)> = Vec::new();
        for (list, names) in [
            ("READ_TOOLS", READ_TOOLS),
            ("WRITE_TOOLS", WRITE_TOOLS),
            ("BUILD_TOOLS", BUILD_TOOLS),
        ] {
            unknown.extend(
                names
                    .iter()
                    .filter(|n| !known.iter().any(|k| k == *n))
                    .map(|n| (list, *n)),
            );
        }
        assert!(
            unknown.is_empty(),
            "allow-listed tools that do not exist: {unknown:?}"
        );
        assert!(
            WRITE_TOOLS.contains(&"write_file"),
            "the execution phase must be able to create files"
        );
    }

    /// A tool description must describe that tool and nothing else.
    ///
    /// Naming a sibling makes the pair a unit that has to be changed together,
    /// and nothing enforces that: `shell` spent an unknown time telling the
    /// model "Find files -> use glob tool" when no tool has ever been called
    /// `glob`. A redirect to nothing is worse than none at all - it forbids
    /// the obvious approach and names an alternative that cannot be called, so
    /// the model invents. "List the files here" became `find_file` against a
    /// guessed root, and before that a `read_file` on `/current_directory`.
    ///
    /// Cross-cutting policy ("prefer edit_file over sed") is not lost by this
    /// rule, it is relocated: it belongs to the system prompt, which states it
    /// once, instead of being restated in every sibling's description and
    /// charged to the context window on every single request.
    #[test]
    fn a_tool_description_never_names_another_tool() {
        let tools = agent_tools();
        let names: Vec<String> = tools.iter().map(|(n, ..)| n.clone()).collect();

        let mut offenders: Vec<String> = Vec::new();
        for (name, description, parameters) in &tools {
            let own = own_vocabulary(parameters);
            let mentioned: Vec<&str> = names
                .iter()
                .filter(|other| *other != name && !AMBIGUOUS.contains(&other.as_str()))
                .filter(|other| !own.contains(other))
                .filter(|other| mentions(description, other))
                .map(String::as_str)
                .collect();
            if !mentioned.is_empty() {
                offenders.push(format!("{name} names {mentioned:?}"));
            }
        }
        assert!(
            offenders.is_empty(),
            "tool descriptions are not self-contained:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// Tool names that are also ordinary English, and so cannot be judged by
    /// spelling alone: `edit_file` says "context line" about unified diffs,
    /// not about the `context` tool. Excluded rather than special-cased per
    /// call site, because a check that reports things that are fine is a check
    /// people learn to override.
    const AMBIGUOUS: &[&str] = &["context", "memory", "system", "task", "todo", "skill"];

    /// Every tool the system prompt names must be a tool that exists.
    ///
    /// Same failure as a description naming a sibling, one level up and more
    /// expensive: the system prompt is sent on every request, so a tool that
    /// was renamed or removed goes on being advertised to the model forever.
    /// It told the model to keep memory with `update_memory` and to pass
    /// `workdir` to `run_terminal_command` - one renamed to `memory`, the
    /// other deleted as a duplicate of `shell`.
    #[test]
    fn the_system_prompt_only_names_tools_that_exist() {
        let known: Vec<String> = agent_tools().into_iter().map(|(n, ..)| n).collect();
        let message = AgentRuntimeContext::default()
            .build_system_message(AgentMode::Agent)
            .expect("the default context builds a system prompt");
        let sven_model::MessageContent::Text(prompt) = message.content else {
            panic!("a system prompt is plain text");
        };

        // Only backticked words are considered: the prompt is prose about
        // software, and `shell` or `task` in a sentence is usually English.
        // A tool it means for the model to CALL is written as code.
        let mut unknown: Vec<String> = Vec::new();
        for span in prompt.split('`').skip(1).step_by(2) {
            let span: &str = span;
            let word = span.trim();
            let looks_like_a_tool = word.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                && word.contains('_')
                && !word.is_empty();
            if looks_like_a_tool && !known.iter().any(|k| k == word) && !NOT_TOOLS.contains(&word) {
                unknown.push(word.to_string());
            }
        }
        unknown.sort();
        unknown.dedup();
        assert!(
            unknown.is_empty(),
            "the system prompt names tools that do not exist: {unknown:?}"
        );
    }

    /// Backticked snake_case in the system prompt that is deliberately not a
    /// tool name - argument names and file names read the same way.
    const NOT_TOOLS: &[&str] = &[
        "max_results",
        "whole_project",
        "output_mode",
        "context_lines",
    ];

    /// Whether `description` refers to the tool `needle` in prose.
    ///
    /// Two things are deliberately not matches. A longer tool's name that
    /// contains a shorter one (`grep` inside `context_grep`) is a different
    /// tool, so the match must land on word boundaries. And a backticked span
    /// is code, not a reference: `grep -E 'error:'` inside a shell example is
    /// the Unix program the caller will actually run, which is exactly the
    /// kind of concrete example a self-contained description should keep.
    fn mentions(description: &str, needle: &str) -> bool {
        let prose = strip_code_spans(description);
        let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        prose.match_indices(needle).any(|(i, _)| {
            let before = prose[..i].chars().next_back();
            let after = prose[i + needle.len()..].chars().next();
            !before.is_some_and(is_word) && !after.is_some_and(is_word)
        })
    }

    /// `description` with every `` `backticked` `` span removed. An unclosed
    /// backtick swallows the rest, which is the safe direction: it can only
    /// hide a reference, never invent one, and the missing pair is the typo to
    /// fix first anyway.
    fn strip_code_spans(description: &str) -> String {
        description
            .split('`')
            .step_by(2)
            .collect::<Vec<_>>()
            .join(" ")
    }
}
