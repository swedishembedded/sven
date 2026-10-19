// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! **sven-team** - agent team coordination library.
//!
//! This crate provides:
//!
//! - [`task`]: Shared task list data model and file-locked storage.
//! - [`config`]: Team configuration (members, roles, metadata).
//! - [`definition`]: Declarative team definition files (`.sven/teams/*.yaml`).
//! - [`cli`]: Implementations for the `sven team` CLI subcommands.

pub mod cli;
pub mod config;
pub mod definition;
pub mod limits;
pub mod task;

// Re-export the most commonly used types.
pub use config::{
    teammate_stable_peer_id, MemberStatus, TeamConfig, TeamConfigStore, TeamMember, TeamRole,
};
pub use definition::{TeamDefinition, TeamMemberDef};
pub use limits::{MemberLimits, TokenAllowance};
pub use task::{default_team_dir, Task, TaskList, TaskStatus, TaskStore, TaskStoreError};
