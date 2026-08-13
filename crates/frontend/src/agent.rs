// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Background agent task and request/event channel types.
//!
//! This module is shared by all Sven frontends (TUI and GUI). It provides the
//! `AgentRequest` enum and the `kernel_session_task` that drives a full
//! [`RuntimeBuilder`] kernel session (via the shared [`KernelAgentSession`]
//! adapter) and bridges [`UiEvent`](sven_hsm::UiEvent)s back to the existing
//! [`AgentEvent`] renderers so the TUI/GUI require no changes.

use std::sync::Arc;

use futures::StreamExt;
use sven_bootstrap::{KernelAgentSession, McpManager, RuntimeBuilder, RuntimeContext};
use sven_config::{AgentMode, Config, ModelConfig};
use sven_core::AgentEvent;
use sven_input::make_title;
use sven_mcp_client::McpEvent;
use sven_model::{CompletionRequest, Message, ResponseEvent};
use sven_runtime::{SharedAgents, SharedSkills};
use sven_tools::{OutputBufferStore, QuestionRequest, SharedToolDisplays, SharedTools};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tracing::{debug, warn};

/// Request sent from a frontend to the background agent task.
///
/// All model overrides carry an already-resolved `ModelConfig`. The frontend
/// resolves the config via `sven_model::resolve_model_from_config`; the agent
/// task only calls `sven_model::from_config` to instantiate the provider,
/// never re-derives which model to use.
#[derive(Debug)]
pub enum AgentRequest {
    /// Submit a new user message (normal flow).
    Submit {
        content: String,
        /// Pre-resolved model config; agent calls `from_config` to instantiate.
        model_override: Option<ModelConfig>,
        mode_override: Option<AgentMode>,
    },
    /// Replace conversation history and submit (edit-and-resubmit flow).
    Resubmit {
        messages: Vec<Message>,
        new_user_content: String,
        /// Pre-resolved model config; agent calls `from_config` to instantiate.
        model_override: Option<ModelConfig>,
        mode_override: Option<AgentMode>,
    },
    /// Pre-load conversation history (resume flow). Does not trigger a model
    /// call; the agent is just primed for the next submission.
    LoadHistory(Vec<Message>),
    /// Generate a short chat title from the first user message (LLM, low
    /// max_tokens, no tools). Result is sent as `AgentEvent::TitleGenerated`.
    GenerateTitle { user_text: String },
    /// Request peer list (node-proxy mode); handled by the mux, not the agent task.
    ListPeers,
    /// Refresh MCP tools from the manager (e.g. when ToolsChanged fires).
    RefreshMcpTools,
    /// One-tap `/share`: expose THIS running session to a broker so a remote
    /// consultant can steer it. Handled by spawning the in-process share bridge
    /// against the live [`RuntimeHandle`](sven_bootstrap::RuntimeHandle).
    ShareSession(Box<crate::share::FrontendShareOptions>),
}

/// Lightweight helper to generate a title from a given model configuration.
async fn generate_title_with_config(cfg: &ModelConfig, user_text: &str) -> Option<String> {
    const TITLE_MAX_TOKENS: u32 = 50;

    let title_model = match sven_model::from_config(cfg) {
        Ok(m) => m,
        Err(e) => {
            warn!(provider = %cfg.provider, model = %cfg.name, error = %e, "title model init failed");
            return None;
        }
    };

    let req = CompletionRequest {
        messages: vec![
            Message::system(
                "Generate a very short conversation title (3-5 words, no quotes). Reply with only the title.",
            ),
            Message::user(user_text.trim()),
        ],
        tools: vec![],
        stream: true,
        system_dynamic_suffix: None,
        cache_key: None,
        max_output_tokens_override: Some(TITLE_MAX_TOKENS),
        core_tool_count: 0,
        response_format: None,
    };

    match title_model.complete(req).await {
        Ok(mut stream) => {
            let mut text = String::new();
            while let Some(ev) = stream.next().await {
                match ev {
                    Ok(ResponseEvent::TextDelta(d)) => text.push_str(&d),
                    Ok(ResponseEvent::Done | ResponseEvent::MaxTokens) => break,
                    Ok(ResponseEvent::ThinkingDelta(_)) => (),
                    Err(e) => {
                        warn!(error = %e, "title generation stream error");
                        return None;
                    }
                    _ => {}
                }
            }
            let t = text.trim().trim_matches('"').trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Err(e) => {
            warn!(error = %e, "title generation request failed");
            None
        }
    }
}

// ── Kernel session task ───────────────────────────────────────────────────────

/// Background task that owns an HSM-kernel session and forwards its
/// [`AgentEvent`] stream back to the frontend.
///
/// Builds a full [`RuntimeBuilder`] session for the requested mode, then wraps
/// it in the shared [`KernelAgentSession`] adapter, which subscribes to the
/// kernel's outward observation bus and bridges [`UiEvent`](sven_hsm::UiEvent)s
/// into the existing [`AgentEvent`] renderers plus relays kernel `AskUser` /
/// approval prompts to the frontend's [`QuestionRequest`] modal channel. The
/// TUI/GUI therefore keep consuming the exact same `AgentEvent` contract.
///
/// The startup model is passed as an already-resolved `ModelConfig` (the
/// frontend applied the CLI `--model` override before spawning). Per-message
/// model overrides in `AgentRequest` variants are also pre-resolved
/// `ModelConfig` values; this task tracks the current one so title generation
/// uses the active model.
///
/// `cancel_handle` is a shared slot holding the sender half of a per-submission
/// cancellation channel; it is wired into the kernel session at build time so
/// the frontend can interrupt the in-flight turn.
/// Optional test seam: map a `(ModelConfig, AgentMode)` to a concrete provider.
///
/// Production passes `None` and the kernel builds the provider from config via
/// `sven_model::from_config`. Tests inject distinguishable mock providers to
/// assert that a model / mode override actually re-drives the kernel through
/// the intended provider.
type ProviderFactory =
    Box<dyn Fn(&ModelConfig, AgentMode) -> Option<Box<dyn sven_model::ModelProvider>> + Send + Sync>;

/// Optional test seam: supply the MCP tool set a `RefreshMcpTools` request
/// installs. Production passes `None` and the tools are pulled from the live
/// [`McpManager`].
type McpToolsSource = Box<dyn Fn() -> Vec<Arc<dyn sven_tools::Tool>> + Send + Sync>;

#[allow(clippy::too_many_arguments)]
pub async fn kernel_session_task(
    config: Arc<Config>,
    startup_model_cfg: ModelConfig,
    mode: AgentMode,
    rx: mpsc::Receiver<AgentRequest>,
    tx: mpsc::Sender<AgentEvent>,
    question_tx: mpsc::Sender<QuestionRequest>,
    cancel_handle: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    shared_skills: SharedSkills,
    shared_agents: SharedAgents,
    shared_tools: SharedTools,
    shared_tool_displays: SharedToolDisplays,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    mcp_manager_tx: Option<oneshot::Sender<(Arc<McpManager>, mpsc::Receiver<McpEvent>)>>,
    mcp_refresh_rx: Option<broadcast::Receiver<()>>,
) {
    run_kernel_session_task(
        config,
        startup_model_cfg,
        mode,
        rx,
        tx,
        question_tx,
        cancel_handle,
        shared_skills,
        shared_agents,
        shared_tools,
        shared_tool_displays,
        buffer_store,
        mcp_manager_tx,
        mcp_refresh_rx,
        None,
        None,
    )
    .await
}

/// `true` if two model configs select a different provider/model/endpoint.
///
/// `ModelConfig` is not `PartialEq`; comparing the provider, model name and
/// base URL is sufficient to detect a per-message model override that requires
/// re-driving the kernel through a different provider.
fn model_cfg_changed(a: &ModelConfig, b: &ModelConfig) -> bool {
    a.provider != b.provider || a.name != b.name || a.base_url != b.base_url
}

/// Build (or rebuild) a fully-wired [`KernelAgentSession`] for `(mode,
/// model_cfg)`, seeded with `history` and bridged into `tx`.
///
/// When `shared_mcp` is `Some`, the existing [`McpManager`] is reused so the
/// frontend's manager handle and MCP connections stay valid across rebuilds.
#[allow(clippy::too_many_arguments)]
async fn build_kernel_session(
    config: &Arc<Config>,
    ctx: &RuntimeContext,
    mode: AgentMode,
    model_cfg: &ModelConfig,
    history: Vec<Message>,
    question_tx: &mpsc::Sender<QuestionRequest>,
    cancel_handle: &Arc<Mutex<Option<oneshot::Sender<()>>>>,
    shared_mcp: Option<Arc<McpManager>>,
    provider_factory: Option<&ProviderFactory>,
    tx: &mpsc::Sender<AgentEvent>,
) -> Result<(KernelAgentSession, mpsc::Receiver<McpEvent>), String> {
    let kernel_mode = mode_to_kernel_mode(mode);
    let mut builder = RuntimeBuilder::new(config.clone(), kernel_mode)
        .with_runtime_context(ctx.clone())
        .with_model_config(model_cfg.clone())
        .with_agent_mode(mode)
        .with_tool_question_tx(question_tx.clone())
        .with_cancel_handle(cancel_handle.clone())
        .with_initial_history(history);
    if let Some(mcp) = shared_mcp {
        builder = builder.with_mcp_manager(mcp);
    }
    if let Some(factory) = provider_factory {
        if let Some(provider) = factory(model_cfg, mode) {
            builder = builder.with_model_provider(provider);
        }
    }
    let bundle = builder
        .build_session()
        .await
        .map_err(|e| format!("kernel session init: {e:#}"))?;
    Ok(KernelAgentSession::spawn(
        bundle,
        tx.clone(),
        question_tx.clone(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_kernel_session_task(
    config: Arc<Config>,
    startup_model_cfg: ModelConfig,
    mode: AgentMode,
    mut rx: mpsc::Receiver<AgentRequest>,
    tx: mpsc::Sender<AgentEvent>,
    question_tx: mpsc::Sender<QuestionRequest>,
    cancel_handle: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    shared_skills: SharedSkills,
    shared_agents: SharedAgents,
    _shared_tools: SharedTools,
    _shared_tool_displays: SharedToolDisplays,
    _buffer_store: Arc<Mutex<OutputBufferStore>>,
    mcp_manager_tx: Option<oneshot::Sender<(Arc<McpManager>, mpsc::Receiver<McpEvent>)>>,
    _mcp_refresh_rx: Option<broadcast::Receiver<()>>,
    provider_factory: Option<ProviderFactory>,
    mcp_tools_source: Option<McpToolsSource>,
) {
    // Reuses the caller's already-discovered skills/agents (the TUI discovers
    // them once at startup) instead of re-walking the search hierarchy here.
    let ctx = RuntimeContext::auto_detect_with(
        sven_runtime::find_project_root().ok(),
        shared_skills,
        shared_agents,
    );

    // Build the initial session. `shared_mcp = None` so the builder constructs
    // and connects the session's own McpManager, which is then shared across
    // any later rebuilds and handed to the frontend below.
    let (mut session, mcp_event_rx) = match build_kernel_session(
        &config,
        &ctx,
        mode,
        &startup_model_cfg,
        Vec::new(),
        &question_tx,
        &cancel_handle,
        None,
        provider_factory.as_ref(),
        &tx,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => {
            let _ = tx.send(AgentEvent::Error(e)).await;
            return;
        }
    };

    // The one McpManager reused across rebuilds so the TUI's handle stays valid.
    let shared_mcp = session.mcp_manager();

    // Unblock the TUI's `mcp_rx.await` by sending the manager immediately.
    // Without this the render loop never starts (blank screen).
    if let Some(mcp_tx) = mcp_manager_tx {
        let _ = mcp_tx.send((shared_mcp.clone(), mcp_event_rx));
    }

    // Track the active mode + model config: title generation uses the current
    // model, and a differing override triggers a session rebuild.
    let mut current_model_cfg = startup_model_cfg;
    let mut current_mode = mode;

    loop {
        let req = match rx.recv().await {
            Some(r) => r,
            None => break,
        };

        match req {
            AgentRequest::Submit {
                content,
                model_override,
                mode_override,
            } => {
                let new_mode = mode_override.unwrap_or(current_mode);
                let new_model = model_override.unwrap_or_else(|| current_model_cfg.clone());
                // A mid-session mode/model change is honoured by rebuilding the
                // kernel with the new policy/provider, seeded with the history
                // accumulated so far so context carries across the switch.
                if new_mode != current_mode || model_cfg_changed(&new_model, &current_model_cfg) {
                    let history = session.history_snapshot();
                    match build_kernel_session(
                        &config,
                        &ctx,
                        new_mode,
                        &new_model,
                        history,
                        &question_tx,
                        &cancel_handle,
                        Some(shared_mcp.clone()),
                        provider_factory.as_ref(),
                        &tx,
                    )
                    .await
                    {
                        Ok((s, _rx)) => session = s,
                        Err(e) => {
                            let _ = tx.send(AgentEvent::Error(e)).await;
                            break;
                        }
                    }
                }
                current_mode = new_mode;
                current_model_cfg = new_model;
                debug!(msg_len = content.len(), "kernel task: posting UserMessage (Submit)");
                if !session.send_user_message(content).await {
                    let _ = tx
                        .send(AgentEvent::Error("kernel queue closed".into()))
                        .await;
                    break;
                }
            }

            AgentRequest::Resubmit {
                messages,
                new_user_content,
                model_override,
                mode_override,
            } => {
                debug!("kernel task: resubmit");
                let new_mode = mode_override.unwrap_or(current_mode);
                let new_model = model_override.unwrap_or_else(|| current_model_cfg.clone());
                // `messages` is the frontend's authoritative reconstructed
                // history (it supports edit-resubmit). The next turn must see
                // exactly it — either by seeding the live store, or, when the
                // mode/model also changed, by seeding the rebuilt kernel.
                if new_mode != current_mode || model_cfg_changed(&new_model, &current_model_cfg) {
                    match build_kernel_session(
                        &config,
                        &ctx,
                        new_mode,
                        &new_model,
                        messages,
                        &question_tx,
                        &cancel_handle,
                        Some(shared_mcp.clone()),
                        provider_factory.as_ref(),
                        &tx,
                    )
                    .await
                    {
                        Ok((s, _rx)) => session = s,
                        Err(e) => {
                            let _ = tx.send(AgentEvent::Error(e)).await;
                            break;
                        }
                    }
                } else {
                    session.seed_history(messages);
                }
                current_mode = new_mode;
                current_model_cfg = new_model;
                if !session.send_user_message(new_user_content).await {
                    let _ = tx
                        .send(AgentEvent::Error("kernel queue closed".into()))
                        .await;
                    break;
                }
            }

            AgentRequest::LoadHistory(messages) => {
                debug!(n = messages.len(), "kernel task: load history (seeding store)");
                session.seed_history(messages);
            }

            AgentRequest::GenerateTitle { user_text } => {
                let cfg = current_model_cfg.clone();
                let event_tx = tx.clone();
                tokio::spawn(async move {
                    let openrouter_title = {
                        let mut free_cfg = cfg.clone();
                        free_cfg.provider = "openrouter".to_string();
                        free_cfg.name = "openrouter/free".to_string();
                        free_cfg.base_url = None;
                        generate_title_with_config(&free_cfg, &user_text).await
                    };
                    let title = if openrouter_title.is_some() {
                        openrouter_title
                    } else {
                        generate_title_with_config(&cfg, &user_text).await
                    };
                    let final_title = title.unwrap_or_else(|| make_title(&user_text));
                    let _ = event_tx.send(AgentEvent::TitleGenerated(final_title)).await;
                });
            }

            AgentRequest::RefreshMcpTools => {
                // Hot-swap the live tool registry's MCP tools so tools that
                // appeared after startup become usable mid-session without a
                // rebuild. The test seam supplies a fixed set; production pulls
                // the current set from the shared McpManager.
                match &mcp_tools_source {
                    Some(source) => session.tool_registry().replace_mcp_tools(source()),
                    None => session.refresh_mcp_tools().await,
                }
            }

            AgentRequest::ListPeers => {
                // Peer discovery is a node-only concern; not applicable here.
            }

            AgentRequest::ShareSession(opts) => {
                // One-tap share: hand THIS live session's RuntimeHandle to the
                // broker via the in-process bridge. The bridge runs for the life
                // of the share; a `ready` signal tells us when the register was
                // accepted so we surface "Session shared as <id>" only then.
                let handle = session.handle();
                let (ready_tx, ready_rx) = oneshot::channel::<String>();
                let notice_tx = tx.clone();
                tokio::spawn(async move {
                    if let Ok(share_id) = ready_rx.await {
                        let _ = notice_tx
                            .send(sven_core::AgentEvent::TextComplete(format!(
                                "Session shared as {share_id}"
                            )))
                            .await;
                    }
                });
                let err_tx = tx.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        crate::share::run_frontend_share_bridge(handle, *opts, Some(ready_tx)).await
                    {
                        let _ = err_tx
                            .send(sven_core::AgentEvent::Error(format!("share: {e:#}")))
                            .await;
                    }
                });
            }
        }
    }
}

/// Select the kernel mode string for a given [`AgentMode`].
///
/// The `"agent"` / reactive machine handles all coding-oriented modes;
/// `"chat"` and `"sdlc"` map to their dedicated machines.
fn mode_to_kernel_mode(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Chat => "chat",
        AgentMode::Sdlc => "sdlc",
        _ => "agent",
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for the interactive fields the kernel session task must
    //! honour after retiring the legacy Agent loop: mid-session history seeding,
    //! mode override, model override, and MCP refresh. Each drives the real
    //! [`run_kernel_session_task`] via its request/event channels, injecting mock
    //! providers / MCP tools through the test seams.

    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use serde_json::{json, Value};
    use sven_config::{AgentMode, Config, ModelConfig};
    use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ScriptedMockProvider};
    use sven_runtime::{SharedAgents, SharedSkills};
    use sven_tools::{
        policy::ApprovalPolicy, OutputBufferStore, SharedTools, Tool, ToolCall, ToolOutput,
    };
    use tokio::sync::{mpsc, Mutex};
    use tokio::task::JoinHandle;

    use super::*;

    fn test_config() -> Arc<Config> {
        let mut c = Config::default();
        c.model.provider = "mock".into();
        c.model.name = "mock-model".into();
        Arc::new(c)
    }

    fn model_cfg(provider: &str, name: &str) -> ModelConfig {
        ModelConfig {
            provider: provider.to_string(),
            name: name.to_string(),
            base_url: None,
            ..ModelConfig::default()
        }
    }

    #[allow(clippy::type_complexity)]
    fn spawn_task(
        config: Arc<Config>,
        startup_model: ModelConfig,
        mode: AgentMode,
        provider_factory: Option<ProviderFactory>,
        mcp_tools_source: Option<McpToolsSource>,
    ) -> (
        mpsc::Sender<AgentRequest>,
        mpsc::Receiver<AgentEvent>,
        mpsc::Receiver<QuestionRequest>,
        JoinHandle<()>,
    ) {
        let (req_tx, req_rx) = mpsc::channel::<AgentRequest>(16);
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(256);
        let (question_tx, question_rx) = mpsc::channel::<QuestionRequest>(16);
        let cancel = Arc::new(Mutex::new(None));
        let handle = tokio::spawn(run_kernel_session_task(
            config,
            startup_model,
            mode,
            req_rx,
            event_tx,
            question_tx,
            cancel,
            SharedSkills::empty(),
            SharedAgents::empty(),
            SharedTools::empty(),
            sven_tools::SharedToolDisplays::new(),
            Arc::new(Mutex::new(OutputBufferStore::new())),
            None,
            None,
            provider_factory,
            mcp_tools_source,
        ));
        (req_tx, event_rx, question_rx, handle)
    }

    /// Consume events until a `TurnComplete`, returning the concatenated
    /// assistant text of that turn.
    async fn turn_text(rx: &mut mpsc::Receiver<AgentEvent>) -> String {
        let mut text = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(AgentEvent::TextDelta(d))) => text.push_str(&d),
                Ok(Some(AgentEvent::TextComplete(t))) => {
                    if text.is_empty() {
                        text = t;
                    }
                }
                Ok(Some(AgentEvent::TurnComplete)) => break,
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => break,
            }
        }
        text
    }

    /// Consume events until a `TurnComplete`, returning every event of the turn.
    async fn turn_events(rx: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            let done = matches!(ev, AgentEvent::TurnComplete);
            out.push(ev);
            if done {
                break;
            }
        }
        out
    }

    fn request_mentions(req: &CompletionRequest, needle: &str) -> bool {
        req.messages
            .iter()
            .any(|m| m.as_text().map(|t| t.contains(needle)).unwrap_or(false))
    }

    // ── Behaviour 1: history seeding on Resubmit ───────────────────────────────

    /// On `Resubmit`, the next turn must stream against the frontend's edited
    /// history — the replaced content must be gone and the edited content
    /// present. A single provider (no mode/model change → no rebuild) records
    /// the exact request it saw.
    #[tokio::test]
    async fn resubmit_seeds_edited_history_into_next_turn() {
        let provider = ScriptedMockProvider::new(vec![
            vec![ResponseEvent::TextDelta("ack1".into()), ResponseEvent::Done],
            vec![ResponseEvent::TextDelta("ack2".into()), ResponseEvent::Done],
        ]);
        let last_request = Arc::clone(&provider.last_request);
        // The provider is consumed by the single initial build; hand it out once.
        let slot: Arc<std::sync::Mutex<Option<Box<dyn ModelProvider>>>> =
            Arc::new(std::sync::Mutex::new(Some(Box::new(provider))));
        let factory: ProviderFactory = Box::new(move |_cfg, _mode| slot.lock().unwrap().take());

        let (req_tx, mut ev, _q, _h) = spawn_task(
            test_config(),
            model_cfg("mock", "mock-model"),
            AgentMode::Agent,
            Some(factory),
            None,
        );

        req_tx
            .send(AgentRequest::Submit {
                content: "ORIGINAL question".into(),
                model_override: None,
                mode_override: None,
            })
            .await
            .unwrap();
        turn_text(&mut ev).await;

        req_tx
            .send(AgentRequest::Resubmit {
                messages: vec![
                    Message::user("EDITED question"),
                    Message::assistant("prior answer"),
                ],
                new_user_content: "follow up".into(),
                model_override: None,
                mode_override: None,
            })
            .await
            .unwrap();
        turn_text(&mut ev).await;

        let req = last_request.lock().unwrap().clone().expect("a request was sent");
        assert!(
            request_mentions(&req, "EDITED question"),
            "next turn must see the edited history: {:?}",
            req.messages
        );
        assert!(
            !request_mentions(&req, "ORIGINAL question"),
            "replaced history must be absent from the next turn: {:?}",
            req.messages
        );
    }

    // ── Behaviour 2: mode override (Plan blocks writes) ────────────────────────

    /// Drive one turn whose provider proposes a `write_file`, under the given
    /// mode override, and report whether the file was actually written.
    async fn write_happens_under(mode_override: Option<AgentMode>) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mode_probe.txt");
        let args = json!({
            "path": target.to_string_lossy(),
            "text": "written",
            "append": false,
        })
        .to_string();
        let factory: ProviderFactory = Box::new(move |_cfg, _mode| {
            Some(Box::new(ScriptedMockProvider::tool_then_text(
                "call-w",
                "write_file",
                args.clone(),
                "done",
            )))
        });

        let (req_tx, mut ev, _q, _h) = spawn_task(
            test_config(),
            model_cfg("mock", "mock-model"),
            AgentMode::Agent,
            Some(factory),
            None,
        );
        req_tx
            .send(AgentRequest::Submit {
                content: "please write".into(),
                model_override: None,
                mode_override,
            })
            .await
            .unwrap();
        turn_events(&mut ev).await;
        target.exists()
    }

    /// `mode_override = Plan` must run the turn in plan mode, where the kernel
    /// forbids `WriteFile`; agent mode executes the same call.
    #[tokio::test]
    async fn mode_override_plan_blocks_write_while_agent_allows() {
        assert!(
            write_happens_under(Some(AgentMode::Agent)).await,
            "agent mode must execute the write_file call"
        );
        assert!(
            !write_happens_under(Some(AgentMode::Plan)).await,
            "plan mode must block the write_file call"
        );
    }

    // ── Behaviour 3: model override switches provider ──────────────────────────

    /// A `model_override` must re-drive generation through the new provider, not
    /// merely relabel the title model.
    #[tokio::test]
    async fn model_override_switches_generation_provider() {
        let factory: ProviderFactory = Box::new(|cfg: &ModelConfig, _mode| {
            let reply = if cfg.name == "model-b" {
                "REPLY_FROM_B"
            } else {
                "REPLY_FROM_A"
            };
            Some(Box::new(ScriptedMockProvider::always_text(reply)))
        });

        let (req_tx, mut ev, _q, _h) = spawn_task(
            test_config(),
            model_cfg("mock", "model-a"),
            AgentMode::Agent,
            Some(factory),
            None,
        );

        req_tx
            .send(AgentRequest::Submit {
                content: "hi".into(),
                model_override: None,
                mode_override: None,
            })
            .await
            .unwrap();
        let t1 = turn_text(&mut ev).await;
        assert!(t1.contains("REPLY_FROM_A"), "turn 1 uses provider A: {t1:?}");

        req_tx
            .send(AgentRequest::Submit {
                content: "again".into(),
                model_override: Some(model_cfg("mock", "model-b")),
                mode_override: None,
            })
            .await
            .unwrap();
        let t2 = turn_text(&mut ev).await;
        assert!(
            t2.contains("REPLY_FROM_B"),
            "model override must switch to provider B, got: {t2:?}"
        );
    }

    // ── Behaviour 4: MCP refresh makes a new tool usable ───────────────────────

    struct FakeMcpTool;

    #[async_trait]
    impl Tool for FakeMcpTool {
        fn name(&self) -> &str {
            "mcp_refreshed_tool"
        }
        fn description(&self) -> &str {
            "a tool that only exists after an MCP refresh"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        fn is_mcp(&self) -> bool {
            true
        }
        async fn execute(&self, _call: &ToolCall) -> ToolOutput {
            ToolOutput::ok("call-m", "REFRESHED_TOOL_OK")
        }
    }

    /// After `RefreshMcpTools`, a newly-available MCP tool must be usable in the
    /// next turn (executed by the live registry, not rejected as unknown).
    #[tokio::test]
    async fn refresh_mcp_tools_makes_new_tool_usable() {
        let factory: ProviderFactory = Box::new(|_cfg, _mode| {
            Some(Box::new(ScriptedMockProvider::tool_then_text(
                "call-m",
                "mcp_refreshed_tool",
                "{}",
                "done",
            )))
        });
        let mcp_tools: McpToolsSource =
            Box::new(|| vec![Arc::new(FakeMcpTool) as Arc<dyn Tool>]);

        let (req_tx, mut ev, _q, _h) = spawn_task(
            test_config(),
            model_cfg("mock", "mock-model"),
            AgentMode::Agent,
            Some(factory),
            Some(mcp_tools),
        );

        req_tx.send(AgentRequest::RefreshMcpTools).await.unwrap();
        req_tx
            .send(AgentRequest::Submit {
                content: "use the tool".into(),
                model_override: None,
                mode_override: None,
            })
            .await
            .unwrap();

        let events = turn_events(&mut ev).await;
        let finished: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCallFinished {
                    tool_name,
                    output,
                    is_error,
                    ..
                } if tool_name == "mcp_refreshed_tool" => Some((output.clone(), *is_error)),
                _ => None,
            })
            .collect();
        assert!(
            finished
                .iter()
                .any(|(output, is_error)| !is_error && output.contains("REFRESHED_TOOL_OK")),
            "refreshed MCP tool must execute successfully, got: {finished:?}"
        );
    }
}
