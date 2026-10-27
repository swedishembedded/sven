// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Inspector overlay: a reusable full-screen pager for browsing sven internals.
//!
//! The inspector wraps [`PagerOverlay`] with a typed [`InspectorKind`] that
//! controls the header title and from which the content `StyledLines` are
//! derived.  All four inspector views (skills, subagents, peers, context)
//! share the same widget, key-handling, and search integration.
//!
//! # Usage
//!
//! ```text
//! /skills    → InspectorKind::Skills
//! /subagents → InspectorKind::Subagents
//! /peers     → InspectorKind::Peers
//! /context   → InspectorKind::Context
//! /tools     → InspectorKind::Tools
//! ```

use std::sync::Arc;

use tokio::sync::Mutex;

use chrono::Local;
use sven_mcp_client::ServerStatusSummary;
use sven_tool_api::format_tools_list;
use sven_tool_registry::ToolSchema;
use sven_tools_fs::OutputBufferStore;
use sven_workspace::{
    find_workspace_root, format_agents_list, format_skills_tree, AgentInfo, SkillInfo,
};

use crate::markdown::{render_markdown, StyledLines};
use crate::pager::PagerOverlay;
use crate::ui::width_utils::truncate_to_width_exact;

// ── InspectorKind ─────────────────────────────────────────────────────────────

/// Re-exported from `sven_frontend::commands` for use in the TUI.
pub use sven_frontend::commands::InspectorKind;

// ── InspectorOverlay ──────────────────────────────────────────────────────────

/// Full-screen scrollable inspector for sven internals.
///
/// Wraps [`PagerOverlay`] and delegates all rendering and key-handling to it.
pub struct InspectorOverlay {
    /// Inner pager that does all the work.
    pub pager: PagerOverlay,
}

impl InspectorOverlay {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// A viewer over arbitrary content - a tool's full output, a message -
    /// under `title`.
    pub fn for_content(lines: StyledLines, title: impl Into<String>) -> Self {
        let mut pager = PagerOverlay::with_title(lines, title);
        pager.scroll_to_line(0);
        Self { pager }
    }

    /// Build the skills inspector from a slice of discovered skills.
    pub fn for_skills(skills: &[SkillInfo], ascii: bool) -> Self {
        let md = format_skills_tree(skills);
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Skills.title()),
        }
    }

    /// Build the subagents inspector from a slice of discovered agents.
    pub fn for_subagents(agents: &[AgentInfo], ascii: bool) -> Self {
        let md = format_agents_list(agents);
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Subagents.title()),
        }
    }

    /// Build the peers inspector.
    ///
    /// Shows discovered subagents (statically configured, available as slash
    /// commands) and any active subprocess buffers from the `OutputBufferStore`.
    pub fn for_peers(
        configured_agents: &[AgentInfo],
        buffer_store: Option<Arc<Mutex<OutputBufferStore>>>,
        ascii: bool,
    ) -> Self {
        let md = format_peers_markdown(configured_agents, buffer_store);
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Peers.title()),
        }
    }

    /// Build the context inspector from runtime session state.
    ///
    /// Shows date/time, project root, workspace root, and active buffer handles.
    pub fn for_context(
        project_root: Option<&std::path::Path>,
        buffer_store: Option<Arc<Mutex<OutputBufferStore>>>,
        ascii: bool,
    ) -> Self {
        let md = format_context_markdown(project_root, buffer_store);
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Context.title()),
        }
    }

    /// Build the tools inspector from the shared registry snapshot.
    pub fn for_tools(tools: &[ToolSchema], ascii: bool) -> Self {
        let md = format_tools_list(tools);
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Tools.title()),
        }
    }

    /// Build the MCP servers inspector from a list of server status summaries.
    pub fn for_mcp(statuses: &[ServerStatusSummary], ascii: bool) -> Self {
        let md = format_mcp_markdown(statuses);
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Mcp.title()),
        }
    }
}

// ── Content renderers ─────────────────────────────────────────────────────────

/// Render the peers view as markdown.
///
/// Shows two sections:
/// 1. Configured subagents - agents discovered from `agents/` dirs.
/// 2. Active subprocess buffers - subagents currently running via `task`.
fn format_peers_markdown(
    configured_agents: &[AgentInfo],
    buffer_store: Option<Arc<Mutex<OutputBufferStore>>>,
) -> String {
    let mut out = String::from("## Peers\n\n");

    // ── Configured subagents ─────────────────────────────────────────────────
    out.push_str("### Configured Subagents\n\n");
    if configured_agents.is_empty() {
        out.push_str("_No subagents configured._\n\n");
    } else {
        for agent in configured_agents {
            out.push_str(&format!("**{}**", agent.name));
            if !agent.description.is_empty() {
                let short = agent.description.trim();
                let preview = truncate_to_width_exact(short, 80);
                out.push_str(&format!(" - {preview}"));
            }
            let mut flags = Vec::new();
            if let Some(ref m) = agent.model {
                flags.push(format!("model:{m}"));
            }
            if agent.readonly {
                flags.push("readonly".to_string());
            }
            if agent.is_background {
                flags.push("background".to_string());
            }
            if !flags.is_empty() {
                out.push_str(&format!(" `{}`", flags.join(" ")));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    // ── Active subprocess buffers ─────────────────────────────────────────────
    out.push_str("### Active Subprocess Buffers\n\n");

    let metadata = buffer_store
        .as_ref()
        .and_then(|store| store.try_lock().ok())
        .map(|guard| guard.list_metadata())
        .unwrap_or_default();

    if metadata.is_empty() {
        out.push_str("_No active subprocess buffers._\n\n");
    } else {
        for meta in &metadata {
            let status_icon = match &meta.status {
                sven_tools_fs::BufferStatus::Running { .. } => "⟳",
                sven_tools_fs::BufferStatus::Finished { exit_code } => {
                    if *exit_code == 0 {
                        "✓"
                    } else {
                        "✗"
                    }
                }
                sven_tools_fs::BufferStatus::Failed { .. } => "✗",
            };
            let elapsed = format_elapsed(meta.elapsed_secs);
            out.push_str(&format!(
                "**{}** {} `{}` - {} lines, {} bytes, {}\n",
                meta.handle_id,
                status_icon,
                meta.status.label(),
                meta.total_lines,
                meta.total_bytes,
                elapsed,
            ));
            if !meta.description.is_empty() {
                out.push_str(&format!("  _{}_\n", meta.description));
            }
        }
        out.push('\n');
    }

    out
}

/// Render the context overview as markdown.
fn format_context_markdown(
    project_root: Option<&std::path::Path>,
    buffer_store: Option<Arc<Mutex<OutputBufferStore>>>,
) -> String {
    let mut out = String::from("## Context\n\n");

    // ── Runtime context ───────────────────────────────────────────────────────
    out.push_str("### Runtime\n\n");
    out.push_str(&format!(
        "**Date/Time:** `{}`\n\n",
        Local::now().format("%Y-%m-%d %H:%M:%S %Z")
    ));
    if let Some(root) = project_root {
        out.push_str(&format!("**Project root:** `{}`\n\n", root.display()));
        let workspace_root = find_workspace_root(root);
        if workspace_root != root {
            out.push_str(&format!(
                "**Workspace root:** `{}`\n\n",
                workspace_root.display()
            ));
            out.push_str(
                "**Relative paths** supplied by the user are resolved relative to the \
                 workspace root unless they begin with the project root.\n\n",
            );
        } else {
            out.push_str("**Workspace root:** _same as project root_\n\n");
            out.push_str(
                "**Relative paths** supplied by the user are resolved relative to the \
                 project root.\n\n",
            );
        }
    } else {
        out.push_str("**Project root:** _not detected_\n\n");
        out.push_str("**Workspace root:** _not detected_\n\n");
    }

    // ── Output buffers ────────────────────────────────────────────────────────
    out.push_str("### Output Buffers\n\n");

    let metadata = buffer_store
        .as_ref()
        .and_then(|store| store.try_lock().ok())
        .map(|guard| guard.list_metadata())
        .unwrap_or_default();

    if metadata.is_empty() {
        out.push_str("_No output buffers active._\n\n");
    } else {
        for meta in &metadata {
            let status_label = meta.status.label();
            let elapsed = format_elapsed(meta.elapsed_secs);
            out.push_str(&format!("#### `{}`\n\n", meta.handle_id,));
            out.push_str(&format!("- **Status:** {status_label}\n"));
            out.push_str(&format!("- **Lines:** {}\n", meta.total_lines));
            out.push_str(&format!("- **Bytes:** {}\n", meta.total_bytes));
            out.push_str(&format!("- **Elapsed:** {elapsed}\n"));
            if !meta.description.is_empty() {
                out.push_str(&format!("- **Source:** {}\n", meta.description));
            }
            if let sven_tools_fs::BufferStatus::Running { pid: Some(pid) } = &meta.status {
                out.push_str(&format!("- **PID:** {pid}\n"));
            }
            if let sven_tools_fs::BufferStatus::Finished { exit_code } = &meta.status {
                out.push_str(&format!("- **Exit code:** {exit_code}\n"));
            }
            if let sven_tools_fs::BufferStatus::Failed { error } = &meta.status {
                out.push_str(&format!("- **Error:** {error}\n"));
            }
            out.push('\n');
        }
    }

    out
}

/// Render the MCP server list as markdown for the inspector.
fn format_mcp_markdown(statuses: &[ServerStatusSummary]) -> String {
    let mut out = String::from("## MCP Servers\n\n");

    if statuses.is_empty() {
        out.push_str(
            "_No MCP servers configured._\n\n\
             Add servers to your `sven.yaml` config under `mcp_servers:`.\n\n\
             **Example (stdio server):**\n\
             ```yaml\n\
             mcp_servers:\n\
               github:\n\
                 transport:\n\
                   type: stdio\n\
                   command: npx\n\
                   args: [\"-y\", \"@modelcontextprotocol/server-github\"]\n\
                 env:\n\
                   GITHUB_TOKEN: \"${GITHUB_TOKEN}\"\n\
             ```\n\n\
             **Example (HTTP server with OAuth):**\n\
             ```yaml\n\
             mcp_servers:\n\
               atlassian:\n\
                 transport:\n\
                   type: http\n\
                   url: \"https://mcp.atlassian.com/v1/mcp\"\n\
                 oauth: {}\n\
             ```\n\n\
             **Default:** sven://sven.mcp/callback (installed by .deb). In containers,\n\
             sven falls back to localhost; forward port 5598.\n\
             **Example (Atlassian with cursor:// - pre-allowlisted):**\n\
             ```yaml\n\
             mcp_servers:\n\
               atlassian:\n\
                 transport:\n\
                   type: http\n\
                   url: \"https://mcp.atlassian.com/v1/mcp\"\n\
                 oauth:\n\
                   redirect_uri: \"cursor://cursor.mcp/callback\"\n\
                   callback_port: 5598\n\
             ```\n",
        );
        return out;
    }

    out.push_str("| Server | Status | Tools | Prompts |\n");
    out.push_str("|--------|--------|-------|---------|\n");

    for s in statuses {
        let icon = s.status_icon();
        let status = s.status.label();
        let detail = match &s.status {
            sven_mcp_client::ServerStatus::Failed { error } => {
                format!(" - _{}_", truncate_to_width_exact(error, 50))
            }
            sven_mcp_client::ServerStatus::Reconnecting { attempts } => {
                format!(" (attempt {attempts})")
            }
            sven_mcp_client::ServerStatus::NeedsAuth { .. } => {
                format!(" - run `/mcp auth {}`", s.name)
            }
            _ => String::new(),
        };
        out.push_str(&format!(
            "| **{}** | {} {}{} | {} | {} |\n",
            s.name, icon, status, detail, s.tool_count, s.prompt_count,
        ));
    }

    out.push_str("\n**Commands:**\n");
    out.push_str("- `/mcp` - open this inspector\n");
    out.push_str("- `/mcp auth <name>` - authenticate with OAuth (browser will open)\n");
    out.push_str("- `/mcp enable <name>` - enable a server\n");
    out.push_str("- `/mcp disable <name>` - disable a server\n");

    // Show a help hint if any server needs auth.
    let needs_auth: Vec<&str> = statuses
        .iter()
        .filter(|s| matches!(s.status, sven_mcp_client::ServerStatus::NeedsAuth { .. }))
        .map(|s| s.name.as_str())
        .collect();
    if !needs_auth.is_empty() {
        out.push_str("\n**Authentication required:**\n");
        for name in &needs_auth {
            out.push_str(&format!(
                "- `/mcp auth {name}` - opens browser for OAuth login\n"
            ));
        }
    }

    out
}

/// Format elapsed seconds into a human-readable string.
fn format_elapsed(secs: f32) -> String {
    if secs < 60.0 {
        format!("{:.1}s", secs)
    } else if secs < 3600.0 {
        format!("{:.0}m {:.0}s", (secs / 60.0).floor(), secs % 60.0)
    } else {
        format!(
            "{:.0}h {:.0}m",
            (secs / 3600.0).floor(),
            (secs % 3600.0) / 60.0
        )
    }
}
