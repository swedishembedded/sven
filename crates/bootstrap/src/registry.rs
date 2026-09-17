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
//! The registry now exposes 14-15 high-quality compound tools instead of 42
//! individual tools. This reduces the model's decision surface, cuts input
//! token cost, and keeps the Anthropic prefix-cache stable across turns.
//!
//! Individual tools (`gdb_start_server`, `buf_read`, etc.) are preserved as
//! Rust types for testing and internal use but are no longer registered as
//! separate entries in the tool registry.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_config::{AgentMode, Config};
use sven_model::ModelProvider;
use sven_tools::{
    events::{TodoItem, ToolEvent},
    ToolRegistry,
};
use sven_tools_agent::{
    AskQuestionTool, ModelCatalogEntry, QuestionRequest, SkillTool, SystemTool, TodoTool,
};
use sven_tools_ctx::{ContextStore, MemoryTool};
use sven_tools_exec::ShellTool;
use sven_tools_fs::{
    AttachFileTool, EditFileTool, FindFileTool, OutputBufferStore, ReadFileTool, WriteTool,
};
#[cfg(all(unix, feature = "gdb"))]
use sven_tools_gdb::GdbSessionState;
use sven_tools_web::{GrepTool, WebFetchTool, WebSearchTool};
use sven_workspace::Shared;

use sven_machines::AgentRuntimeContext;

use crate::context::ToolSetProfile;
use crate::context_tool::ContextTool;
use crate::task_tool::TaskTool;
#[cfg(all(unix, feature = "gdb"))]
use crate::GdbTool;

// ── Integration tool providers ────────────────────────────────────────────────

/// Optional providers for integration tools.
///
/// All fields are optional; tools are only registered when their provider is set.
#[derive(Default)]
pub struct IntegrationProviders {
    /// Channel manager for the `send_message` tool.
    #[cfg(feature = "integrations")]
    pub channel_manager: Option<sven_channels::ChannelManager>,

    /// Job store for the `schedule` tool.
    #[cfg(feature = "integrations")]
    pub job_store: Option<Arc<sven_scheduler::JobStore>>,

    /// Email provider for the `email` tool.
    #[cfg(feature = "integrations")]
    pub email: Option<Arc<dyn sven_integrations::email::EmailProvider>>,

    /// Calendar provider for the `calendar` tool.
    #[cfg(feature = "integrations")]
    pub calendar: Option<Arc<dyn sven_integrations::calendar::CalendarProvider>>,

    /// TTS provider for the `voice` tool.
    #[cfg(feature = "integrations")]
    pub tts: Option<Arc<dyn sven_integrations::voice::TtsProvider>>,

    /// STT provider for the `voice` tool.
    #[cfg(feature = "integrations")]
    pub stt: Option<Arc<dyn sven_integrations::voice::SttProvider>>,

    /// Voice call provider for the `voice` tool.
    #[cfg(feature = "integrations")]
    pub calls: Option<Arc<dyn sven_integrations::voice::VoiceCallProvider>>,

    /// Semantic memory store for the `semantic_memory` tool.
    #[cfg(feature = "memory")]
    pub memory_store: Option<Arc<dyn sven_memory::VectorStore>>,

    /// Durable pending-facts ledger for the `assimilate_fact` tool.
    #[cfg(feature = "memory")]
    pub fact_ledger: Option<sven_memory::PendingFactsLedger>,

    /// Session-scoped provenance the `assimilate_fact` tool resolves evidence
    /// handles against.
    #[cfg(feature = "memory")]
    pub provenance_index: Option<Arc<sven_memory::ProvenanceIndex>>,

    /// Human approvals observed by the kernel's user executor, read by the
    /// `assimilate_fact` tool before admitting web-sourced content.
    #[cfg(feature = "memory")]
    pub knowledge_approvals: Option<Arc<sven_vocab::provenance::KnowledgeApprovals>>,
}

/// Converts the model catalog into the slice-of-fields `SystemTool`'s
/// `switch_model` fuzzy search needs, without giving `sven-tools` a direct
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
/// The `buffer_store` is now bundled inside the `profile` variants that need it
/// (`Full`, `Coding`, `SubAgent`).
///
/// Pass `integrations` to register the messaging, email, calendar, voice, and
/// memory tools. All fields are optional; only providers that are `Some` get
/// registered.
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
/// integration tools (messaging, email, calendar, voice, memory) when providers
/// are supplied.
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
            question_tx,
            todos,
            buffer_store,
        } => build_profile_full(FullProfileParams {
            cfg,
            model,
            mode_lock,
            question_tx,
            todos,
            tool_event_tx,
            runtime: &sub_agent_runtime,
            buffer_store,
            include_gdb_context: true,
        }),
        ToolSetProfile::Coding {
            question_tx,
            todos,
            buffer_store,
        } => build_profile_full(FullProfileParams {
            cfg,
            model,
            mode_lock,
            question_tx,
            todos,
            tool_event_tx,
            runtime: &sub_agent_runtime,
            buffer_store,
            include_gdb_context: false,
        }),
        ToolSetProfile::Research { question_tx, todos } => build_profile_research(
            cfg,
            model,
            mode_lock,
            question_tx,
            todos,
            tool_event_tx,
            &sub_agent_runtime,
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
    // Integration tools are registered when the `integrations` feature is enabled
    // and providers are supplied via IntegrationProviders.
    //
    // Without the feature enabled this is a no-op; the providers struct has no fields.
    #[cfg(feature = "integrations")]
    {
        if let Some(manager) = _providers.channel_manager {
            _reg.register(sven_channels::SendMessageTool::new(manager));
        }
        if let Some(store) = _providers.job_store {
            _reg.register(sven_scheduler::ScheduleTool::new(store));
        }
        if let Some(provider) = _providers.email {
            _reg.register(sven_integrations::email::EmailTool::new(provider));
        }
        if let Some(provider) = _providers.calendar {
            _reg.register(sven_integrations::calendar::CalendarTool::new(provider));
        }
        if _providers.tts.is_some() || _providers.stt.is_some() || _providers.calls.is_some() {
            _reg.register(sven_integrations::voice::VoiceTool::new(
                _providers.tts,
                _providers.stt,
                _providers.calls,
            ));
        }
    }
    // `memory` is a separate feature from `integrations` (see this crate's
    // Cargo.toml) precisely so a `minimal` build can exclude `rusqlite`
    // while every other build keeps semantic memory on by default.
    #[cfg(feature = "memory")]
    {
        if let Some(store) = _providers.memory_store {
            // One scope per assembled registry - that is, per session. Both
            // memory tools share it: `assimilate_fact` stamps it onto records
            // that must not outlive this session (an unapproved web fetch),
            // and `semantic_memory` is the only tool that can then recall them.
            let scope = sven_memory::SessionScope::new();
            _reg.register(
                sven_memory::SemanticMemoryTool::new(Arc::clone(&store))
                    .with_session_scope(scope.clone()),
            );
            // `assimilate_fact` is the single writer into durable knowledge:
            // it needs the same memory store plus the ledger it gates writes
            // into. Without a ledger there is nothing to gate, so it is not
            // registered at all rather than silently degrading to a second
            // ungated memory writer.
            if let Some(ledger) = _providers.fact_ledger {
                // `ingest_document` is the entry point for learning from a
                // document the user hands over: it records the digest that
                // makes `assimilate_fact`'s `UserProvidedDocument` check
                // admit facts extracted from it. Registered alongside
                // `assimilate_fact`, on the same ledger, since one is
                // pointless without the other.
                _reg.register(sven_memory::IngestDocumentTool::new(ledger.clone()));
                _reg.register(
                    sven_memory::AssimilateFactTool::new(
                        store,
                        ledger,
                        _providers.provenance_index.unwrap_or_default(),
                        _providers.knowledge_approvals.unwrap_or_default(),
                    )
                    .with_session_scope(scope),
                );
            }
        }
    }
}

/// Parameters shared by the Full and Coding profile builders.
struct FullProfileParams<'a> {
    cfg: &'a Config,
    model: Arc<dyn ModelProvider>,
    mode_lock: Arc<Mutex<AgentMode>>,
    question_tx: Option<mpsc::Sender<QuestionRequest>>,
    todos: Arc<Mutex<Vec<TodoItem>>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &'a AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    include_gdb_context: bool,
}

/// Full and Coding profiles share the same builder; `include_gdb_context`
/// controls whether GDB and context tools are included.
fn build_profile_full(p: FullProfileParams<'_>) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    // Capture the model identity before p.model is moved into register_base_tools.
    let model_id = format!("{}/{}", p.model.name(), p.model.model_name());

    register_base_tools(
        &mut reg,
        p.cfg,
        p.model,
        p.mode_lock,
        p.tool_event_tx.clone(),
        p.runtime,
        Arc::clone(&p.buffer_store),
        p.include_gdb_context,
    );

    if let Some(tx) = p.question_tx {
        reg.register(AskQuestionTool::new_tui(tx));
    }
    reg.register(TodoTool::new(p.todos, p.tool_event_tx.clone()));

    reg.register(TaskTool::new(
        Arc::clone(&p.buffer_store),
        p.tool_event_tx,
        Some(model_id),
        p.runtime.agents.clone(),
    ));

    reg
}

/// Research profile: read-only, no write tools, no task spawning.
fn build_profile_research(
    cfg: &Config,
    model: Arc<dyn sven_model::ModelProvider>,
    mode_lock: Arc<Mutex<AgentMode>>,
    question_tx: Option<mpsc::Sender<QuestionRequest>>,
    todos: Arc<Mutex<Vec<TodoItem>>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &AgentRuntimeContext,
) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    // Read-only file tools only.
    reg.register(ReadFileTool);
    reg.register(FindFileTool);
    reg.register(GrepTool);
    reg.register(WebFetchTool::new(cfg.tools.web.fetch_max_chars));
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });
    reg.register(MemoryTool::new(
        cfg.tools.memory.memory_file.clone(),
        runtime.knowledge.clone(),
    ));
    reg.register(SkillTool::new(runtime.skills.clone()));
    reg.register(SystemTool::new(
        mode_lock,
        tool_event_tx.clone(),
        model_catalog_for_tools(),
    ));

    if let Some(tx) = question_tx {
        reg.register(AskQuestionTool::new_tui(tx));
    }
    reg.register(TodoTool::new(todos, tool_event_tx.clone()));

    // Task is included for delegation but limited to research mode.
    let buffer_store = Arc::new(Mutex::new(OutputBufferStore::new()));
    reg.register(TaskTool::new(
        buffer_store,
        tool_event_tx,
        Some(format!("{}/{}", model.name(), model.model_name())),
        runtime.agents.clone(),
    ));

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

    register_base_tools(
        &mut reg,
        cfg,
        model,
        mode_lock,
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
    mode_lock: Arc<Mutex<AgentMode>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    include_full: bool,
) {
    // ── File I/O ─────────────────────────────────────────────────────────────
    // read_file already handles images (auto-detected by extension).
    reg.register(ReadFileTool);
    reg.register(FindFileTool);
    reg.register(WriteTool);
    reg.register_with_display(EditFileTool);

    // ── Multimodal attachments ───────────────────────────────────────────────
    // attach_file needs the live model to decide whether audio can be sent
    // natively or must be transcribed, so clone the Arc before `model` is
    // moved into the context tool below.
    reg.register(AttachFileTool::new(
        Some(Arc::clone(&model)),
        cfg.tools.asr.clone(),
    ));

    // ── Search ────────────────────────────────────────────────────────────────
    // grep now supports whole_project=true (replaces search_codebase).
    reg.register(GrepTool);

    // ── Shell ─────────────────────────────────────────────────────────────────
    // shell covers: run commands, delete files, list dirs, run linters.
    reg.register(ShellTool {
        timeout_secs: cfg.tools.timeout_secs,
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

    // ── System (mode + model switching) ──────────────────────────────────────
    reg.register(SystemTool::new(
        mode_lock,
        tool_event_tx.clone(),
        model_catalog_for_tools(),
    ));

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
            reg.register(GdbTool::new(gdb_state, cfg.tools.gdb.clone()));
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
    reg.register(ReadFileTool);
    reg.register(FindFileTool);
    reg.register(WriteTool);
    reg.register_with_display(EditFileTool);

    // ── Multimodal attachments ───────────────────────────────────────────────
    // No live model in the CLI registry, so audio is always transcribed —
    // the only answer that is correct for every possible target model.
    reg.register(AttachFileTool::new(None, cfg.tools.asr.clone()));

    // ── Search ────────────────────────────────────────────────────────────────
    reg.register(GrepTool);

    // ── Web ───────────────────────────────────────────────────────────────────
    reg.register(WebFetchTool::new(cfg.tools.web.fetch_max_chars));
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });

    // ── System ────────────────────────────────────────────────────────────────
    reg.register(ShellTool {
        timeout_secs: cfg.tools.timeout_secs,
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
        reg.register(GdbTool::new(gdb_state, cfg.tools.gdb.clone()));
    }

    reg
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model_mock::MockProvider;

    /// Every tool the agent is offered, with its description.
    fn agent_tools() -> Vec<(String, String, serde_json::Value)> {
        let (tx, _rx) = mpsc::channel::<ToolEvent>(16);
        let reg = build_tool_registry(
            &Config::default(),
            Arc::new(MockProvider),
            ToolSetProfile::Full {
                question_tx: None,
                todos: Arc::new(Mutex::new(Vec::new())),
                buffer_store: Arc::new(Mutex::new(OutputBufferStore::default())),
            },
            Arc::new(Mutex::new(AgentMode::Agent)),
            tx,
            AgentRuntimeContext::default(),
        );
        reg.schemas().into_iter().map(|s| (s.name, s.description, s.parameters)).collect()
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
            let variants = spec.get("enum").and_then(|e| e.as_array()).into_iter().flatten();
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
        let expected = [
            "attach_file", "context", "edit_file", "find_file", "gdb", "grep", "memory",
            "read_file", "shell", "skill", "system", "task", "todo", "web_fetch",
            "web_search", "write_file",
        ];
        assert_eq!(got, expected, "the agent's tool set changed");
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
        assert!(offenders.is_empty(), "tool descriptions are not self-contained:\n  {}", offenders.join("\n  "));
    }

    /// Tool names that are also ordinary English, and so cannot be judged by
    /// spelling alone: `edit_file` says "context line" about unified diffs,
    /// not about the `context` tool. Excluded rather than special-cased per
    /// call site, because a check that reports things that are fine is a check
    /// people learn to override.
    const AMBIGUOUS: &[&str] = &["context", "memory", "system", "task", "todo", "skill"];

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
