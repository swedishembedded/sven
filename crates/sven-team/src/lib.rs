// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! **sven-team** — agent team coordination library.
//!
//! This crate provides:
//!
//! - [`task`]: Shared task list data model and file-locked storage.
//! - [`config`]: Team configuration (members, roles, metadata).
//! - [`tools`]: LLM-callable tools for the task list (`create_task`, `claim_task`, …).
//! - [`spawn`]: LLM-callable tools for team lifecycle (`create_team`, `list_team`, …).
//! - [`prompts`]: Orchestrator system prompt fragments.
//! - [`definition`]: Declarative team definition files (`.sven/teams/*.yaml`).
//! - [`cli`]: Implementations for the `sven team` CLI subcommands.

extern crate libc;

pub mod cli;
pub mod config;
pub mod definition;
pub mod prompts;
pub mod spawn;
pub mod task;
pub mod tools;
pub mod worktree;

// Re-export the most commonly used types.
pub use config::{MemberStatus, TeamConfig, TeamConfigStore, TeamMember, TeamRole};
pub use definition::{TeamDefinition, TeamMemberDef};
pub use spawn::{
    CleanupTeamTool, CreateTeamTool, ListTeamTool, MergeTeammateBranchTool, RegisterTeammateTool,
    ShutdownTeammateTool, SpawnTeammateTool, TeamConfigHandle,
};
pub use task::{default_team_dir, Task, TaskList, TaskStatus, TaskStore, TaskStoreError};
pub use tools::{
    AssignTaskTool, ClaimTaskTool, CompleteTaskTool, CreateTaskTool, ListTasksTool,
    TaskStoreHandle, UpdateTaskTool,
};
pub use worktree::{
    create_teammate_worktree, find_repo_root, list_team_worktrees, merge_teammate_branch,
    remove_worktree, teammate_branch_name, teammate_worktree_path, WorktreeGuard,
};
