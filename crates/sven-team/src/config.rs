// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Team configuration: members, roles, and team metadata.
//!
//! Stored at `~/.config/sven/teams/{team-name}/config.json`.

use std::{fs, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::task::default_team_dir;

/// Role of a peer within a team.
///
/// Roles are informational — they appear in the team picker and can be
/// referenced in the orchestrator prompt, but they do not restrict tool
/// access.  The lead decides task assignments; the LLM uses role hints to
/// pick the right teammate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    /// The lead coordinates work and synthesizes results.
    Lead,
    /// General-purpose teammate.
    Teammate,
    /// Focused on implementation.
    Implementer,
    /// Focused on code or design review.
    Reviewer,
    /// Focused on research or exploration.
    Explorer,
    /// Runs tests and validates outcomes.
    Tester,
    /// Free-form role label.
    Custom(String),
}

impl Default for TeamRole {
    fn default() -> Self {
        TeamRole::Teammate
    }
}

impl std::fmt::Display for TeamRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamRole::Lead => write!(f, "lead"),
            TeamRole::Teammate => write!(f, "teammate"),
            TeamRole::Implementer => write!(f, "implementer"),
            TeamRole::Reviewer => write!(f, "reviewer"),
            TeamRole::Explorer => write!(f, "explorer"),
            TeamRole::Tester => write!(f, "tester"),
            TeamRole::Custom(s) => write!(f, "{s}"),
        }
    }
}

/// Status of a teammate as last reported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    #[default]
    Unknown,
    Active,
    Idle,
    Closed,
}

impl std::fmt::Display for MemberStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemberStatus::Unknown => write!(f, "unknown"),
            MemberStatus::Active => write!(f, "active"),
            MemberStatus::Idle => write!(f, "idle"),
            MemberStatus::Closed => write!(f, "closed"),
        }
    }
}

/// A single team member record stored in the team config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    /// libp2p peer ID (base58).
    pub peer_id: String,
    /// Human-readable name of the agent.
    pub name: String,
    /// Role within the team.
    pub role: TeamRole,
    /// LLM model hint (e.g. `"claude-sonnet-4-5"`).  `None` = same as lead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Current status (populated at runtime, not persisted).
    #[serde(default, skip_serializing_if = "is_default_status")]
    pub status: MemberStatus,
    /// Current task ID being worked on (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_task_id: Option<String>,
    /// When this member was added to the team.
    pub joined_at: DateTime<Utc>,
}

fn is_default_status(s: &MemberStatus) -> bool {
    matches!(s, MemberStatus::Unknown)
}

/// Top-level team configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    /// Team name (also used as the directory name).
    pub name: String,
    /// Peer ID of the lead (the session that created the team).
    pub lead_peer_id: String,
    /// All team members including the lead.
    pub members: Vec<TeamMember>,
    /// When the team was created.
    pub created_at: DateTime<Utc>,
    /// Optional description of the team's overall goal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// Maximum number of simultaneous active teammates (default: 8).
    #[serde(default = "default_max_active")]
    pub max_active: usize,
    /// Global token budget for the entire team (0 = unlimited).
    #[serde(default)]
    pub token_budget: u64,
    /// Running total of tokens consumed across all team members.
    #[serde(default)]
    pub tokens_used: u64,
    /// Maximum number of agent-loop iterations allowed per teammate task.
    /// 0 = unlimited.
    #[serde(default)]
    pub max_iterations: u32,
}

fn default_max_active() -> usize {
    8
}

impl TeamConfig {
    /// Create a new team config with just the lead.
    pub fn new(
        name: impl Into<String>,
        lead_peer_id: impl Into<String>,
        lead_name: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            lead_peer_id: lead_peer_id.into(),
            members: vec![TeamMember {
                peer_id: String::new(), // filled by caller
                name: lead_name.into(),
                role: TeamRole::Lead,
                model: None,
                status: MemberStatus::Active,
                current_task_id: None,
                joined_at: Utc::now(),
            }],
            created_at: Utc::now(),
            goal: None,
            max_active: default_max_active(),
            token_budget: 0,
            tokens_used: 0,
            max_iterations: 0,
        }
    }

    /// Find a member by peer ID.
    pub fn find_member(&self, peer_id: &str) -> Option<&TeamMember> {
        self.members.iter().find(|m| m.peer_id == peer_id)
    }

    /// Check whether `peer_id` is the lead.
    pub fn is_lead(&self, peer_id: &str) -> bool {
        self.lead_peer_id == peer_id
    }

    /// Returns `true` if the token budget is exhausted.
    /// Always returns `false` when `token_budget == 0` (unlimited).
    pub fn budget_exhausted(&self) -> bool {
        self.token_budget > 0 && self.tokens_used >= self.token_budget
    }
}

/// File-backed team config store.
pub struct TeamConfigStore {
    path: PathBuf,
}

impl TeamConfigStore {
    /// Open (or create) the store for `team_name`.
    pub fn open(team_name: &str) -> Result<Self, anyhow::Error> {
        let dir = default_team_dir(team_name);
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("config.json"),
        })
    }

    /// Open the store at an explicit path (for tests).
    pub fn open_at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Load the config from disk.  Returns `None` when the file does not exist.
    pub fn load(&self) -> Result<Option<TeamConfig>, anyhow::Error> {
        if !self.path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&self.path)?;
        Ok(Some(serde_json::from_str(&data)?))
    }

    /// Persist the config to disk (overwrites any existing file).
    pub fn save(&self, config: &TeamConfig) -> Result<(), anyhow::Error> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(config)?;
        fs::write(&self.path, data)?;
        Ok(())
    }

    /// Modify the config with a closure.  Writes back on success.
    pub fn modify<F>(&self, f: F) -> Result<(), anyhow::Error>
    where
        F: FnOnce(&mut TeamConfig),
    {
        let mut config = self
            .load()?
            .ok_or_else(|| anyhow::anyhow!("team config not found at {:?}", self.path))?;
        f(&mut config);
        self.save(&config)
    }

    /// Atomically add `tokens` to the team's running token total.
    ///
    /// Safe to call concurrently from multiple teammates because it uses the
    /// same file-locked RMW pattern as `modify`.
    pub fn record_token_usage(&self, tokens: u64) -> Result<(), anyhow::Error> {
        self.modify(|config| {
            config.tokens_used = config.tokens_used.saturating_add(tokens);
        })
    }
}
