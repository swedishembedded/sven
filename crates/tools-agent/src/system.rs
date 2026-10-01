// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Compound `system` tool that consolidates agent self-modification capabilities.
//!
//! Actions:
//! - `switch_mode`       - switch the agent's operating mode, within the modes
//!   the session may use (never wider than the one it started in).
//! - `switch_model`      - change the active LLM using an fzf-style fuzzy search string.
//! - `add_mcp_server`    - add an MCP server to the nearest config file.
//! - `remove_mcp_server` - remove an MCP server from the nearest config file.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use sven_config::{AgentMode, McpOAuthConfig, McpServerConfig, McpTransport};

use sven_hsm::ToolCapability;

use sven_tool_api::events::ToolEvent;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};

/// The `provider`/`id`/`name` slice of a model catalog entry that
/// `switch_model`'s fuzzy search needs. A local type rather than depending on
/// `sven_model_catalog::ModelCatalogEntry` directly: the tool layer has no
/// other reason to know about the model-provider crate, and the catalog is
/// static data the caller already has (via `sven_model_catalog::static_catalog()`)
/// when it constructs a [`SystemTool`].
#[derive(Debug, Clone)]
pub struct ModelCatalogEntry {
    pub id: String,
    pub name: String,
    pub provider: String,
}

/// Compound system tool - mode, model, and MCP server management.
pub struct SystemTool {
    current_mode: Arc<Mutex<AgentMode>>,
    event_tx: mpsc::Sender<ToolEvent>,
    model_catalog: Vec<ModelCatalogEntry>,
    /// The modes `switch_mode` may switch to; `None` for any.
    allowed_modes: Option<Vec<AgentMode>>,
    /// Whether `add_mcp_server`/`remove_mcp_server` are offered at all.
    manages_mcp: bool,
}

impl SystemTool {
    pub fn new(
        current_mode: Arc<Mutex<AgentMode>>,
        event_tx: mpsc::Sender<ToolEvent>,
        model_catalog: Vec<ModelCatalogEntry>,
    ) -> Self {
        Self {
            current_mode,
            event_tx,
            model_catalog,
            allowed_modes: None,
            manages_mcp: true,
        }
    }

    /// Lets `switch_mode` switch only to `modes`: the model may narrow what
    /// the session may do, never widen it past where it started.
    #[must_use]
    pub fn with_allowed_modes(mut self, modes: Vec<AgentMode>) -> Self {
        self.allowed_modes = Some(modes);
        self
    }

    /// Withholds `add_mcp_server` and `remove_mcp_server`: they write the
    /// configuration and start an arbitrary command, which a read-only or
    /// delegated session must not do.
    #[must_use]
    pub fn without_mcp_management(mut self) -> Self {
        self.manages_mcp = false;
        self
    }

    /// Refuses an MCP-server action where it may not run: withheld from this
    /// session, or the session is not in `agent` mode (the only one that both
    /// writes files and runs commands).
    async fn refuse_mcp_action(&self, call: &ToolCall) -> Option<ToolOutput> {
        if !self.manages_mcp {
            return Some(ToolOutput::err(
                &call.id,
                "MCP servers cannot be managed from this session",
            ));
        }
        let mode = *self.current_mode.lock().await;
        (mode != AgentMode::Agent).then(|| {
            ToolOutput::err(
                &call.id,
                format!("MCP servers can be managed only in agent mode, not in {mode} mode"),
            )
        })
    }

    async fn exec_switch_mode(&self, call: &ToolCall) -> ToolOutput {
        let mode_str = match call.args.get("mode").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'mode' for action=switch_mode"),
        };

        let target = match mode_str.as_str() {
            "research" => AgentMode::Research,
            "plan" => AgentMode::Plan,
            "agent" => AgentMode::Agent,
            "chat" => AgentMode::Chat,
            "sdlc" => AgentMode::Sdlc,
            other => return ToolOutput::err(&call.id, format!("unknown mode: {other}")),
        };

        if let Some(allowed) = &self.allowed_modes {
            if !allowed.contains(&target) {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "cannot switch to {target} mode: this session may use only {}",
                        allowed
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }

        // Hold the lock for the entire check-then-write to avoid TOCTOU.
        let mut mode_guard = self.current_mode.lock().await;
        let current = *mode_guard;

        debug!(from = ?current, to = ?target, "system tool switch_mode");

        if current == target {
            return ToolOutput::ok(&call.id, format!("already in {mode_str} mode"));
        }

        *mode_guard = target;
        // Release the lock before awaiting on the channel send.
        drop(mode_guard);
        let _ = self.event_tx.send(ToolEvent::ModeChanged(target)).await;

        ToolOutput::ok(&call.id, format!("switched to {target} mode"))
    }

    async fn exec_add_mcp_server(&self, call: &ToolCall) -> ToolOutput {
        if let Some(refused) = self.refuse_mcp_action(call).await {
            return refused;
        }
        let name = match call.args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'name' parameter"),
        };

        let cfg = build_mcp_config_from_args(&call.args);

        // Write to the nearest writable config file.
        match write_mcp_server_to_config(&name, &cfg) {
            Ok(path) => {
                debug!(server = %name, path = %path.display(), "MCP server added to config");
                let _ = self
                    .event_tx
                    .send(ToolEvent::McpServerAdded {
                        name: name.clone(),
                        config: cfg,
                    })
                    .await;
                ToolOutput::ok(
                    &call.id,
                    format!("Added MCP server '{name}' to {}", path.display()),
                )
            }
            Err(e) => ToolOutput::err(&call.id, format!("failed to write config: {e}")),
        }
    }

    async fn exec_remove_mcp_server(&self, call: &ToolCall) -> ToolOutput {
        if let Some(refused) = self.refuse_mcp_action(call).await {
            return refused;
        }
        let name = match call.args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'name' parameter"),
        };

        match remove_mcp_server_from_config(&name) {
            Ok(path) => {
                debug!(server = %name, path = %path.display(), "MCP server removed from config");
                let _ = self
                    .event_tx
                    .send(ToolEvent::McpServerRemoved(name.clone()))
                    .await;
                ToolOutput::ok(
                    &call.id,
                    format!("Removed MCP server '{name}' from {}", path.display()),
                )
            }
            Err(e) => ToolOutput::err(&call.id, format!("failed to update config: {e}")),
        }
    }

    async fn exec_switch_model(&self, call: &ToolCall) -> ToolOutput {
        let query = match call.args.get("model").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'model' for action=switch_model"),
        };

        debug!(query = %query, "system tool switch_model");

        let best = self
            .model_catalog
            .iter()
            .filter_map(|entry| {
                let candidate = format!("{}/{}", entry.provider, entry.id);
                // Take the maximum score across all searchable fields so that a
                // query like "claude" scores the bare id ("claude-opus") higher
                // than the full form ("anthropic/claude-opus") when both match.
                let score = [
                    fuzzy_score(&query, &candidate),
                    fuzzy_score(&query, &entry.id),
                    fuzzy_score(&query, &entry.name),
                ]
                .into_iter()
                .flatten()
                .max();
                score.map(|s| (s, candidate))
            })
            .max_by_key(|(score, _)| *score);

        match best {
            Some((_, model_str)) => {
                let _ = self
                    .event_tx
                    .send(ToolEvent::ModelChanged(model_str.clone()))
                    .await;
                ToolOutput::ok(&call.id, format!("switching model to {model_str}"))
            }
            None => ToolOutput::err(
                &call.id,
                format!(
                    "no model matched '{query}'. Use a fragment of the model or provider name."
                ),
            ),
        }
    }
}

#[async_trait]
impl Tool for SystemTool {
    fn name(&self) -> &str {
        "system"
    }

    fn description(&self) -> &str {
        "Agent system controls: mode/model switching and MCP server management.\n\
         action: switch_mode | switch_model | add_mcp_server | remove_mcp_server\n\n\
         switch_mode: Switch operating mode (research, plan, agent) within the\n\
         modes this session may use.\n\n\
         switch_model: Switch the active LLM (e.g. \"claude-opus\", \"gpt4o\").\n\n\
         add_mcp_server: Add an external MCP server. Writes to the nearest config file.\n\
           - stdio: provide command + args (e.g. npx -y @modelcontextprotocol/server-github)\n\
           - http: provide url (e.g. https://mcp.example.com/v1)\n\n\
         remove_mcp_server: Remove an MCP server by name from the nearest config file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["switch_mode", "switch_model", "add_mcp_server", "remove_mcp_server"],
                    "description": "Which system action to perform"
                },
                "mode": {
                    "type": "string",
                    "enum": ["research", "plan", "agent"],
                    "description": "[switch_mode] Target operating mode"
                },
                "model": {
                    "type": "string",
                    "description": "[switch_model] Fuzzy search string (e.g. 'claude-opus')"
                },
                "name": {
                    "type": "string",
                    "description": "[add/remove_mcp_server] Server name used as tool prefix"
                },
                "command": {
                    "type": "string",
                    "description": "[add_mcp_server stdio] Executable to run"
                },
                "args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "[add_mcp_server stdio] Arguments to the command"
                },
                "url": {
                    "type": "string",
                    "description": "[add_mcp_server http] HTTP URL of the MCP server"
                },
                "env": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": "[add_mcp_server] Environment variables for stdio transport"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    /// The widest action, `add_mcp_server`, starts a command.
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }

    /// Switching mode or model changes only the session; removing an MCP
    /// server writes the configuration; adding one also starts its command.
    fn call_capability(&self, args: &Value) -> ToolCapability {
        sven_tool_api::capability_by_action(
            args,
            &[
                ("switch_mode", ToolCapability::ReadFile),
                ("switch_model", ToolCapability::ReadFile),
                ("remove_mcp_server", ToolCapability::WriteFile),
                ("add_mcp_server", ToolCapability::ExecuteShell),
            ],
            ToolCapability::ExecuteShell,
        )
    }

    // Available in all modes: switch_model has no mode restriction, and
    // switch_mode enforces the downgrade-only rule internally.

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'action'"),
        };

        match action.as_str() {
            "switch_mode" => self.exec_switch_mode(call).await,
            "switch_model" => self.exec_switch_model(call).await,
            "add_mcp_server" => self.exec_add_mcp_server(call).await,
            "remove_mcp_server" => self.exec_remove_mcp_server(call).await,
            other => ToolOutput::err(
                &call.id,
                format!(
                    "unknown action '{other}'. Valid: switch_mode, switch_model, \
                     add_mcp_server, remove_mcp_server"
                ),
            ),
        }
    }
}

// ── MCP config helpers ────────────────────────────────────────────────────────

/// Build a [`McpServerConfig`] from the `add_mcp_server` tool call arguments.
fn build_mcp_config_from_args(args: &Value) -> McpServerConfig {
    let url = args.get("url").and_then(|v| v.as_str()).map(str::to_string);

    let transport = if let Some(url) = url {
        let headers = args
            .get("headers")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        McpTransport::Http { url, headers }
    } else {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("npx")
            .to_string();
        let cmd_args: Vec<String> = args
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        McpTransport::Stdio {
            command,
            args: cmd_args,
        }
    };

    let env: std::collections::HashMap<String, String> = args
        .get("env")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let oauth = args
        .get("oauth_scopes")
        .and_then(|v| v.as_array())
        .map(|scopes| McpOAuthConfig {
            scopes: scopes
                .iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect(),
            client_id: None,
            client_secret: None,
            redirect_uri: None,
            callback_port: None,
        });

    McpServerConfig {
        transport,
        enabled: true,
        env,
        oauth,
        timeout_secs: 30,
    }
}

/// Find the nearest writable config file to modify.
///
/// Preference order (first found wins):
/// 1. `.sven/config.yaml` in CWD
/// 2. `.sven.yaml` in CWD
/// 3. `sven.yaml` in CWD
/// 4. `~/.config/sven/config.yaml` (create if missing)
fn nearest_writable_config() -> PathBuf {
    let candidates = [
        PathBuf::from(".sven/config.yaml"),
        PathBuf::from(".sven.yaml"),
        PathBuf::from("sven.yaml"),
    ];
    for p in &candidates {
        if p.exists() {
            return p.clone();
        }
    }
    // Fall back to ~/.config/sven/config.yaml.
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("sven")
        .join("config.yaml")
}

/// Write an MCP server entry to a YAML config file.
///
/// Parses the existing file (if any), merges the new entry into
/// `mcp_servers.<name>`, and writes it back.  Returns the path written to.
fn write_mcp_server_to_config(name: &str, cfg: &McpServerConfig) -> anyhow::Result<PathBuf> {
    let path = nearest_writable_config();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut doc: serde_yaml::Value = if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        serde_yaml::from_str(&raw)?
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    };

    let cfg_value = serde_yaml::to_value(cfg)?;

    // Ensure `mcp_servers` key exists.
    if let serde_yaml::Value::Mapping(ref mut map) = doc {
        let key = serde_yaml::Value::String("mcp_servers".to_string());
        let servers = map
            .entry(key)
            .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        if let serde_yaml::Value::Mapping(ref mut s) = servers {
            s.insert(serde_yaml::Value::String(name.to_string()), cfg_value);
        }
    }

    let yaml_str = serde_yaml::to_string(&doc)?;
    std::fs::write(&path, yaml_str)?;

    Ok(path)
}

/// Remove an MCP server entry from the nearest config file.
fn remove_mcp_server_from_config(name: &str) -> anyhow::Result<PathBuf> {
    let path = nearest_writable_config();

    if !path.exists() {
        anyhow::bail!("no config file found to remove server from");
    }

    let raw = std::fs::read_to_string(&path)?;
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&raw)?;

    if let serde_yaml::Value::Mapping(ref mut map) = doc {
        let key = serde_yaml::Value::String("mcp_servers".to_string());
        if let Some(serde_yaml::Value::Mapping(ref mut servers)) = map.get_mut(&key) {
            servers.remove(serde_yaml::Value::String(name.to_string()));
        }
    }

    let yaml_str = serde_yaml::to_string(&doc)?;
    std::fs::write(&path, yaml_str)?;

    Ok(path)
}

/// Fuzzy subsequence scorer - mirrors `sven-commands`'
/// `completion::fuzzy_score`.  Inlined here because `sven-tools-agent`
/// (domain tier) may not depend on `sven-commands` (assembly tier).
///
/// Returns `None` when `pattern` is not a subsequence of `candidate`.
/// A higher score indicates a better match.  Bonuses:
/// - +1 per matched character
/// - +5 if the first character of `candidate` matches
/// - +3 for each consecutive character match
/// - +2 for a word-boundary match (preceded by `/`, `-`, `_`, or space)
fn fuzzy_score(pattern: &str, candidate: &str) -> Option<usize> {
    if pattern.is_empty() {
        return Some(0);
    }

    let pattern_lc: Vec<char> = pattern.to_lowercase().chars().collect();
    let candidate_lc: Vec<char> = candidate.to_lowercase().chars().collect();

    let mut score = 0usize;
    let mut cand_idx = 0usize;
    let mut prev_matched = false;

    for pat_ch in &pattern_lc {
        let found = candidate_lc[cand_idx..].iter().position(|c| c == pat_ch);
        match found {
            Some(offset) => {
                let actual_idx = cand_idx + offset;
                score += 1;
                if prev_matched && offset == 0 {
                    score += 3;
                }
                if actual_idx == 0 {
                    score += 5;
                }
                if actual_idx > 0 {
                    let prev = candidate_lc[actual_idx - 1];
                    if matches!(prev, '/' | '-' | '_' | ' ') {
                        score += 2;
                    }
                }
                cand_idx = actual_idx + 1;
                prev_matched = offset == 0;
            }
            None => return None,
        }
    }

    Some(score)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc;

    use super::*;
    use sven_tool_api::tool::{Tool, ToolCall};

    fn make_tool(
        mode: AgentMode,
    ) -> (SystemTool, Arc<Mutex<AgentMode>>, mpsc::Receiver<ToolEvent>) {
        let current = Arc::new(Mutex::new(mode));
        let (tx, rx) = mpsc::channel(16);
        let catalog = sven_model_catalog::static_catalog()
            .into_iter()
            .map(|e| ModelCatalogEntry {
                id: e.id,
                name: e.name,
                provider: e.provider,
            })
            .collect();
        let tool = SystemTool::new(current.clone(), tx, catalog);
        (tool, current, rx)
    }

    fn mode_call(mode: &str) -> ToolCall {
        ToolCall {
            id: "s1".into(),
            name: "system".into(),
            args: json!({"action": "switch_mode", "mode": mode}),
        }
    }

    fn model_call(model: &str) -> ToolCall {
        ToolCall {
            id: "s2".into(),
            name: "system".into(),
            args: json!({"action": "switch_model", "model": model}),
        }
    }

    // ── switch_mode tests ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn agent_can_downgrade_to_plan() {
        let (tool, current, _rx) = make_tool(AgentMode::Agent);
        let out = tool.execute(&mode_call("plan")).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(*current.lock().await, AgentMode::Plan);
    }

    #[tokio::test]
    async fn agent_can_downgrade_to_research() {
        let (tool, current, _rx) = make_tool(AgentMode::Agent);
        let out = tool.execute(&mode_call("research")).await;
        assert!(!out.is_error);
        assert_eq!(*current.lock().await, AgentMode::Research);
    }

    #[tokio::test]
    async fn research_can_upgrade_to_agent() {
        let (tool, current, _rx) = make_tool(AgentMode::Research);
        let out = tool.execute(&mode_call("agent")).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(*current.lock().await, AgentMode::Agent);
    }

    #[tokio::test]
    async fn plan_can_upgrade_to_agent() {
        let (tool, current, _rx) = make_tool(AgentMode::Plan);
        let out = tool.execute(&mode_call("agent")).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(*current.lock().await, AgentMode::Agent);
    }

    #[tokio::test]
    async fn research_can_upgrade_to_plan() {
        let (tool, current, _rx) = make_tool(AgentMode::Research);
        let out = tool.execute(&mode_call("plan")).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(*current.lock().await, AgentMode::Plan);
    }

    /// The model may not widen what the session may do: a session that
    /// started read-only stays read-only.
    #[tokio::test]
    async fn switch_mode_stays_within_the_allowed_modes() {
        let (tool, current, _rx) = make_tool(AgentMode::Research);
        let tool = tool.with_allowed_modes(vec![AgentMode::Research, AgentMode::Plan]);
        let out = tool.execute(&mode_call("agent")).await;
        assert!(out.is_error, "{}", out.content);
        assert_eq!(*current.lock().await, AgentMode::Research);
        let out = tool.execute(&mode_call("plan")).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(*current.lock().await, AgentMode::Plan);
    }

    /// Adding an MCP server writes the configuration and starts a command:
    /// refused where the session is not in agent mode or does not manage
    /// servers at all.
    #[tokio::test]
    async fn mcp_servers_are_managed_only_where_that_is_allowed() {
        let add = ToolCall {
            id: "m".into(),
            name: "system".into(),
            args: json!({"action": "add_mcp_server", "name": "x", "command": "evil"}),
        };
        let (tool, _current, _rx) = make_tool(AgentMode::Plan);
        let out = tool.execute(&add).await;
        assert!(
            out.is_error && out.content.contains("agent mode"),
            "{}",
            out.content
        );
        let (tool, _current, _rx) = make_tool(AgentMode::Agent);
        let out = tool.without_mcp_management().execute(&add).await;
        assert!(out.is_error, "{}", out.content);
    }

    /// Each action is held to what it does: adding an MCP server starts a
    /// command, removing one writes the configuration, switching mode or
    /// model changes only the session.
    #[tokio::test]
    async fn system_actions_are_classed_by_effect() {
        let (tool, _current, _rx) = make_tool(AgentMode::Agent);
        assert_eq!(
            tool.call_capability(&json!({"action": "add_mcp_server"})),
            ToolCapability::ExecuteShell
        );
        for action in ["switch_mode", "switch_model"] {
            assert_eq!(
                tool.call_capability(&json!({ "action": action })),
                ToolCapability::ReadFile,
                "{action}"
            );
        }
        assert_eq!(
            tool.call_capability(&json!({"action": "remove_mcp_server"})),
            ToolCapability::WriteFile
        );
    }

    #[tokio::test]
    async fn same_mode_is_noop() {
        let (tool, current, _rx) = make_tool(AgentMode::Agent);
        let out = tool.execute(&mode_call("agent")).await;
        assert!(!out.is_error);
        assert!(out.content.contains("already in"));
        assert_eq!(*current.lock().await, AgentMode::Agent);
    }

    #[tokio::test]
    async fn emits_mode_changed_event() {
        let (tool, _current, mut rx) = make_tool(AgentMode::Agent);
        tool.execute(&mode_call("plan")).await;
        let event = rx.try_recv().expect("should emit event");
        matches!(event, ToolEvent::ModeChanged(AgentMode::Plan));
    }

    #[tokio::test]
    async fn missing_mode_param_is_error() {
        let (tool, _current, _rx) = make_tool(AgentMode::Agent);
        let call = ToolCall {
            id: "1".into(),
            name: "system".into(),
            args: json!({"action": "switch_mode"}),
        };
        let out = tool.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'mode'"));
    }

    #[tokio::test]
    async fn missing_action_is_error() {
        let (tool, _current, _rx) = make_tool(AgentMode::Agent);
        let call = ToolCall {
            id: "1".into(),
            name: "system".into(),
            args: json!({}),
        };
        let out = tool.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'action'"));
    }

    // ── switch_model tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn switch_model_matches_claude() {
        let (tool, _current, mut rx) = make_tool(AgentMode::Agent);
        let out = tool.execute(&model_call("claude-opus")).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("anthropic"));
        let event = rx.try_recv().expect("should emit ModelChanged event");
        matches!(event, ToolEvent::ModelChanged(_));
    }

    #[tokio::test]
    async fn switch_model_available_from_research_mode() {
        let (tool, _current, _rx) = make_tool(AgentMode::Research);
        let out = tool.execute(&model_call("gpt-4o")).await;
        assert!(!out.is_error, "{}", out.content);
    }

    #[tokio::test]
    async fn switch_model_no_match_is_error() {
        let (tool, _current, _rx) = make_tool(AgentMode::Agent);
        let out = tool.execute(&model_call("zzzznotamodel")).await;
        assert!(out.is_error);
        assert!(out.content.contains("no model matched"));
    }

    #[tokio::test]
    async fn switch_model_missing_model_param_is_error() {
        let (tool, _current, _rx) = make_tool(AgentMode::Agent);
        let call = ToolCall {
            id: "1".into(),
            name: "system".into(),
            args: json!({"action": "switch_model"}),
        };
        let out = tool.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'model'"));
    }

    #[tokio::test]
    async fn emits_model_changed_event() {
        let (tool, _current, mut rx) = make_tool(AgentMode::Agent);
        tool.execute(&model_call("gpt-4o")).await;
        let event = rx.try_recv().expect("should emit event");
        matches!(event, ToolEvent::ModelChanged(_));
    }

    #[tokio::test]
    async fn switch_model_prefers_bare_id_score_over_full_form() {
        // "claude" scores higher against "claude-opus" (start-of-string bonus)
        // than against "anthropic/claude-opus" (word-boundary bonus only).
        // The max-across-fields logic should pick the better score.
        let (tool, _current, _rx) = make_tool(AgentMode::Agent);
        let out = tool.execute(&model_call("claude-opus")).await;
        assert!(!out.is_error, "{}", out.content);
        // Should resolve to an anthropic claude-opus variant.
        assert!(out.content.contains("anthropic"));
        assert!(out.content.contains("claude-opus") || out.content.contains("claude"));
    }

    // ── fuzzy_score unit tests ────────────────────────────────────────────────

    #[test]
    fn fuzzy_score_exact_match() {
        assert!(fuzzy_score("gpt", "gpt-4o").is_some());
    }

    #[test]
    fn fuzzy_score_subsequence_match() {
        assert!(fuzzy_score("claude", "anthropic/claude-opus-4-6").is_some());
    }

    #[test]
    fn fuzzy_score_no_match() {
        assert!(fuzzy_score("zzz", "anthropic/claude-opus").is_none());
    }

    #[test]
    fn fuzzy_score_empty_pattern_always_matches() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn fuzzy_score_consecutive_bonus() {
        let consecutive = fuzzy_score("gpt", "gpt-4o").unwrap();
        let scattered = fuzzy_score("gpt", "g-p-t-4o").unwrap();
        assert!(consecutive > scattered);
    }

    #[test]
    fn fuzzy_score_start_bonus_beats_word_boundary() {
        // "claude" at start of string scores higher than after "anthropic/".
        let start = fuzzy_score("claude", "claude-opus").unwrap();
        let after_slash = fuzzy_score("claude", "anthropic/claude-opus").unwrap();
        assert!(start > after_slash);
    }
}
