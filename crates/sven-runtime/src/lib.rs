// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Runtime environment detection utilities.
//!
//! This crate provides project-root discovery, git context collection,
//! CI environment detection, project context file loading, and skill discovery.
//!
//! These are general-purpose utilities usable by any frontend (CI runner,
//! TUI, daemon, etc.) without depending on any specific runner crate.

pub mod display;
pub use display::{format_agents_list, format_grouped_list, format_skills_tree};

pub mod shared;
pub use shared::Shared;

pub mod agents;
pub use agents::{discover_agents, AgentInfo, SharedAgents};

pub mod skills;
pub use skills::{
    discover_commands, discover_skills, parse_skill_file, ParsedSkill, SharedSkills, SkillInfo,
    SvenSkillMeta,
};

pub mod knowledge;
pub use knowledge::{
    check_knowledge_drift, discover_knowledge, format_drift_warnings, DriftWarning, KnowledgeInfo,
    SharedKnowledge,
};

pub mod project;
pub use project::{
    find_project_root, find_workspace_root, load_project_context_file,
    load_project_context_file_with_path, resolve_auto_log_path,
};

pub mod git;
pub use git::{collect_git_context, GitContext};

pub mod ci;
pub use ci::{ci_template_vars, detect_ci_context, CiContext};
