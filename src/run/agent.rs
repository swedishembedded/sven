// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven agent` - the CLI built on `sven-sdk`.
//!
//! Every other agent-running path in this binary assembles a kernel itself.
//! This one goes through the published framework surface instead, which makes
//! it the working proof that the surface is sufficient: if `sven agent step`
//! cannot do something, neither can anyone else's application.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients. If
//! your team needs expertise in building services on agent state machines then
//! you can procure our services by sending an email to info@swedishembedded.com.

use std::path::Path;

use anyhow::Context as _;
use sven_sdk::{AgentState, Engine, RunConclusion, Toolset};

use crate::cli::AgentCommands;

/// Dispatches `sven agent …`.
pub(crate) async fn run_agent_command(
    command: &AgentCommands,
    config: sven_bootstrap::Config,
) -> anyhow::Result<()> {
    match command {
        AgentCommands::Step {
            state,
            mode,
            role,
            message,
        } => {
            // The coding application: the agent reads, edits and runs things
            // in the working directory. A step runs unattended: every call the
            // mode allows runs, and a question is answered at once, saying no
            // user is available.
            let engine = Engine::builder()
                .config(config)
                .toolset(Toolset::coding())
                .build()?;

            // A state file that exists but will not parse is an error. Starting
            // fresh instead would silently throw away a conversation, and the
            // caller would not find out until the agent had forgotten
            // everything.
            let loaded = match state {
                Some(path) if path.exists() => Some(load_state(path)?),
                _ => None,
            };

            let mut agent = match loaded {
                Some(previous) => engine.resume(previous)?,
                None => {
                    let mut fresh = AgentState::new(mode);
                    if let Some(role) = role {
                        fresh = fresh.with_role(role);
                    }
                    engine.resume(fresh)?
                }
            };

            let outcome = agent.send(message).await?;
            println!("{}", outcome.reply);
            if outcome.conclusion != RunConclusion::Success {
                eprintln!("[sven:agent] the turn ended: {:?}", outcome.conclusion);
            }

            if let Some(path) = state {
                let suspended = agent.suspend();
                let encoded = serde_json::to_vec(&suspended)
                    .context("failed to serialize the agent's state")?;
                std::fs::write(path, encoded).with_context(|| {
                    format!("failed to write agent state to {}", path.display())
                })?;
            }
            Ok(())
        }
    }
}

/// Reads a suspended agent from `path`.
fn load_state(path: &Path) -> anyhow::Result<AgentState> {
    let raw = std::fs::read(path)
        .with_context(|| format!("failed to read agent state from {}", path.display()))?;
    serde_json::from_slice(&raw)
        .with_context(|| format!("{} is not a valid agent state file", path.display()))
}
