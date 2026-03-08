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

extern crate libc;

pub mod config;
pub mod prompts;
pub mod spawn;
pub mod task;
pub mod tools;

// Re-export the most commonly used types.
pub use config::{MemberStatus, TeamConfig, TeamConfigStore, TeamMember, TeamRole};
pub use spawn::{
    CleanupTeamTool, CreateTeamTool, ListTeamTool, RegisterTeammateTool, ShutdownTeammateTool,
    SpawnTeammateTool, TeamConfigHandle,
};
pub use task::{default_team_dir, Task, TaskList, TaskStatus, TaskStore, TaskStoreError};
pub use tools::{
    AssignTaskTool, ClaimTaskTool, CompleteTaskTool, CreateTaskTool, ListTasksTool,
    TaskStoreHandle, UpdateTaskTool,
};
