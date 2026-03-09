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
use sven_runtime::{
    find_workspace_root, format_agents_list, format_skills_tree, AgentInfo, SkillInfo,
};
use sven_tools::{format_tools_list, OutputBufferStore, ToolSchema};

use crate::markdown::render_markdown;
use crate::pager::PagerOverlay;

// ── InspectorKind ─────────────────────────────────────────────────────────────

/// Identifies which view the inspector is displaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorKind {
    Skills,
    Subagents,
    Peers,
    Context,
    Tools,
}

impl InspectorKind {
    /// Header title shown in the pager banner.
    pub fn title(self) -> &'static str {
        match self {
            InspectorKind::Skills => "SKILLS",
            InspectorKind::Subagents => "SUBAGENTS",
            InspectorKind::Peers => "PEERS",
            InspectorKind::Context => "CONTEXT",
            InspectorKind::Tools => "TOOLS",
        }
    }
}

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

    /// Build the skills inspector from a slice of discovered skills.
    ///
    /// `is_node_proxy` adds a note that skills are locally discovered and may
    /// differ from the connected node's skill set.
    pub fn for_skills(skills: &[SkillInfo], is_node_proxy: bool, ascii: bool) -> Self {
        let mut md = format_skills_tree(skills);
        if is_node_proxy {
            md = format!(
                "> **Connected to node** — skills are discovered from the local \
                 filesystem and may differ from the node's skill set.\n\n{md}"
            );
        }
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Skills.title()),
        }
    }

    /// Build the subagents inspector from a slice of discovered agents.
    ///
    /// `is_node_proxy` adds a note that subagents are locally discovered.
    pub fn for_subagents(agents: &[AgentInfo], is_node_proxy: bool, ascii: bool) -> Self {
        let mut md = format_agents_list(agents);
        if is_node_proxy {
            md = format!(
                "> **Connected to node** — subagents are discovered from the local \
                 filesystem and may differ from the node's subagent set.\n\n{md}"
            );
        }
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Subagents.title()),
        }
    }

    /// Build the peers inspector.
    ///
    /// Shows discovered subagents (statically configured, available as slash
    /// commands) and any active subprocess buffers from the `OutputBufferStore`.
    /// In node-proxy mode the buffer store lives in the node process, so a
    /// note is shown instead of an empty buffer list.
    pub fn for_peers(
        configured_agents: &[AgentInfo],
        buffer_store: Option<Arc<Mutex<OutputBufferStore>>>,
        is_node_proxy: bool,
        ascii: bool,
    ) -> Self {
        let md = format_peers_markdown(configured_agents, buffer_store, is_node_proxy);
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Peers.title()),
        }
    }

    /// Build the context inspector from runtime session state.
    ///
    /// Shows date/time, project root, workspace root, and active buffer handles.
    /// In node-proxy mode the subprocess buffers live in the node process, so a
    /// note is shown instead of potentially empty local data.
    pub fn for_context(
        project_root: Option<&std::path::Path>,
        buffer_store: Option<Arc<Mutex<OutputBufferStore>>>,
        is_node_proxy: bool,
        ascii: bool,
    ) -> Self {
        let md = format_context_markdown(project_root, buffer_store, is_node_proxy);
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Context.title()),
        }
    }

    /// Build the tools inspector.
    ///
    /// In local mode `tools` comes from the shared registry snapshot.
    /// In node-proxy mode the caller fetches the list from the node first via
    /// [`crate::node_agent::fetch_node_tools`] and passes it here.
    pub fn for_tools(tools: &[ToolSchema], is_node_proxy: bool, ascii: bool) -> Self {
        let source_note = if is_node_proxy {
            "> **Connected to node** — showing tools registered on the node.\n\n"
        } else {
            ""
        };
        let md = format!("{}{}", source_note, format_tools_list(tools));
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Tools.title()),
        }
    }
}

// ── Content renderers ─────────────────────────────────────────────────────────

/// Render the peers view as markdown.
///
/// Shows two sections:
/// 1. Configured subagents — agents discovered from `agents/` dirs.
/// 2. Active subprocess buffers — subagents currently running via `task`.
fn format_peers_markdown(
    configured_agents: &[AgentInfo],
    buffer_store: Option<Arc<Mutex<OutputBufferStore>>>,
    is_node_proxy: bool,
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
                let preview = if short.len() > 80 {
                    &short[..80]
                } else {
                    short
                };
                out.push_str(&format!(" — {preview}"));
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

    if is_node_proxy {
        out.push_str(
            "_Subprocess buffers are not available in node-proxy mode — they live \
             in the node process. Use `/tools` to inspect the node's tool set._\n\n",
        );
        return out;
    }

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
                sven_tools::BufferStatus::Running { .. } => "⟳",
                sven_tools::BufferStatus::Finished { exit_code } => {
                    if *exit_code == 0 {
                        "✓"
                    } else {
                        "✗"
                    }
                }
                sven_tools::BufferStatus::Failed { .. } => "✗",
            };
            let elapsed = format_elapsed(meta.elapsed_secs);
            out.push_str(&format!(
                "**{}** {} `{}` — {} lines, {} bytes, {}\n",
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
    is_node_proxy: bool,
) -> String {
    let mut out = String::from("## Context\n\n");

    if is_node_proxy {
        out.push_str(
            "> **Connected to node** — context data below reflects the local TUI session. \
             Subprocess buffers and tool calls are executed in the node process. \
             Use `/tools` to inspect the node's registered tools.\n\n",
        );
    }

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

    if is_node_proxy {
        out.push_str(
            "_Output buffers are not available in node-proxy mode — they live \
             in the node process._\n\n",
        );
        return out;
    }

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
            if let sven_tools::BufferStatus::Running { pid: Some(pid) } = &meta.status {
                out.push_str(&format!("- **PID:** {pid}\n"));
            }
            if let sven_tools::BufferStatus::Finished { exit_code } = &meta.status {
                out.push_str(&format!("- **Exit code:** {exit_code}\n"));
            }
            if let sven_tools::BufferStatus::Failed { error } = &meta.status {
                out.push_str(&format!("- **Error:** {error}\n"));
            }
            out.push('\n');
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
