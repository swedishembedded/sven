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

use ratatui::text::Line;
use sven_frontend::ContextUsage;

use crate::markdown::{render_markdown, StyledLines};
use crate::pager::PagerOverlay;
use crate::ui::context_panel::context_panel_lines;
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
    /// Shows what a request carries against the context window (see
    /// [`context_panel_lines`]), then date/time, project root, workspace root
    /// and active buffer handles.
    pub fn for_context(
        usage: &ContextUsage,
        project_root: Option<&std::path::Path>,
        buffer_store: Option<Arc<Mutex<OutputBufferStore>>>,
        ascii: bool,
    ) -> Self {
        let mut lines = context_panel_lines(usage, ascii);
        lines.push(Line::default());
        lines.extend(render_markdown(
            &format_context_markdown(project_root, buffer_store),
            0,
            ascii,
        ));
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
    let mut out = String::from("## Runtime\n\n");

    // ── Runtime context ───────────────────────────────────────────────────────
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
            "_No MCP servers yet._\n\n\
             Add one, and its tools are offered at once:\n\n\
             - `/mcp add <name> <url>` - a server on the web (it asks you to sign in when it needs to)\n\
             - `/mcp add <name> <command> [args...]` - a server run on this machine\n\n\
             Both are written to your configuration file; `sven mcp add` does the same from a shell.\n",
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
            sven_mcp_client::ServerStatus::Reconnecting { attempts, error } => {
                format!(
                    " (attempt {attempts}) - _{}_",
                    truncate_to_width_exact(error, 50)
                )
            }
            sven_mcp_client::ServerStatus::NeedsAuth { auth_url } if auth_url.is_empty() => {
                format!(" - run `/mcp auth {}`", s.name)
            }
            sven_mcp_client::ServerStatus::NeedsAuth { .. } => {
                " - signing in (link below)".to_string()
            }
            _ => String::new(),
        };
        out.push_str(&format!(
            "| **{}** | {} {}{} | {} | {} |\n",
            s.name, icon, status, detail, s.tool_count, s.prompt_count,
        ));
    }

    // A sign-in waiting for the person: its whole link, to open or copy
    // (on a remote shell the browser sven tried to open is not there).
    let waiting: Vec<(&str, &str)> = statuses
        .iter()
        .filter_map(|s| match &s.status {
            sven_mcp_client::ServerStatus::NeedsAuth { auth_url } if !auth_url.is_empty() => {
                Some((s.name.as_str(), auth_url.as_str()))
            }
            _ => None,
        })
        .collect();
    if !waiting.is_empty() {
        out.push_str("\n**Sign in:**\n");
        for (name, url) in waiting {
            out.push_str(&format!("- `{name}`: <{url}>\n"));
        }
    }

    out.push_str("\n**Commands:**\n");
    out.push_str("- `/mcp add <name> <url | command args...>` - add a server\n");
    out.push_str("- `/mcp remove <name>` - remove it\n");
    out.push_str("- `/mcp auth <name>` - sign in (the browser opens)\n");
    out.push_str("- `/mcp logout <name>` - forget the sign-in\n");
    out.push_str("- `/mcp enable <name>`, `/mcp disable <name>` - offer its tools, or not, for this session\n");

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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use sven_mcp_client::ServerStatus;

    /// The `/mcp` page as the pager draws it, `width` x `height`.
    fn drawn(statuses: &[ServerStatusSummary], width: u16, height: u16) -> String {
        let lines = render_markdown(
            &format_mcp_markdown(statuses),
            width.saturating_sub(2),
            false,
        );
        let mut pager = crate::pager::PagerOverlay::with_title(lines, InspectorKind::Mcp.title());
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| pager.render(f, &[], 0, "", None, false))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A sign-in waiting for the person shows its whole link, however long,
    /// and every command is listed.
    #[test]
    fn a_waiting_sign_in_shows_its_whole_link() {
        let link = "https://auth.example.com/authorize?response_type=code&client_id=sven-abc&state=Zm9vYmFy&code_challenge=YWJj&resource=https%3A%2F%2Fmcp.example.com%2Fmcp";
        let statuses = [
            ServerStatusSummary {
                name: "docs".into(),
                status: ServerStatus::NeedsAuth {
                    auth_url: link.into(),
                },
                tool_count: 0,
                prompt_count: 0,
            },
            ServerStatusSummary {
                name: "github".into(),
                status: ServerStatus::Connected,
                tool_count: 24,
                prompt_count: 2,
            },
        ];
        let screen = drawn(&statuses, 100, 30);
        println!("{screen}");
        let joined: String = screen.lines().map(str::trim).collect();
        assert!(joined.contains(link), "the link is cut:\n{screen}");
        for command in [
            "/mcp add",
            "/mcp remove",
            "/mcp auth",
            "/mcp logout",
            "/mcp enable",
        ] {
            assert!(screen.contains(command), "{command} missing:\n{screen}");
        }
    }

    #[test]
    fn with_no_server_it_says_how_to_add_one() {
        let screen = drawn(&[], 100, 20);
        println!("{screen}");
        assert!(screen.contains("/mcp add <name> <url>"), "{screen}");
        assert!(
            !screen.contains("mcp_servers:"),
            "no YAML to hand-edit:\n{screen}"
        );
    }
}
