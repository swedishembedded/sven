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
//! ```

use std::sync::Arc;

use tokio::sync::Mutex;

use sven_runtime::{format_agents_list, format_skills_tree, AgentInfo, SkillInfo};
use sven_tools::OutputBufferStore;

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
}

impl InspectorKind {
    /// Header title shown in the pager banner.
    pub fn title(self) -> &'static str {
        match self {
            InspectorKind::Skills => "SKILLS",
            InspectorKind::Subagents => "SUBAGENTS",
            InspectorKind::Peers => "PEERS",
            InspectorKind::Context => "CONTEXT",
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
    // ── Sync constructors (skills / subagents / peers use them) ───────────────

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
    /// Shows discovered subagents (the statically configured agents, available
    /// as slash commands) and any active subprocess buffers from the
    /// `OutputBufferStore` if provided.
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
    /// Shows project context, skills/agents counts, and active buffer handles.
    pub fn for_context(
        project_root: Option<&std::path::Path>,
        skills_count: usize,
        agents_count: usize,
        buffer_store: Option<Arc<Mutex<OutputBufferStore>>>,
        ascii: bool,
    ) -> Self {
        let md = format_context_markdown(project_root, skills_count, agents_count, buffer_store);
        let lines = render_markdown(&md, 0, ascii);
        Self {
            pager: PagerOverlay::with_title(lines, InspectorKind::Context.title()),
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
    skills_count: usize,
    agents_count: usize,
    buffer_store: Option<Arc<Mutex<OutputBufferStore>>>,
) -> String {
    let mut out = String::from("## Context\n\n");

    // ── Runtime context ───────────────────────────────────────────────────────
    out.push_str("### Runtime\n\n");
    if let Some(root) = project_root {
        out.push_str(&format!("**Project root:** `{}`\n\n", root.display()));
    } else {
        out.push_str("**Project root:** _not detected_\n\n");
    }
    out.push_str(&format!("**Skills loaded:** {skills_count}\n\n"));
    out.push_str(&format!("**Subagents configured:** {agents_count}\n\n"));

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
