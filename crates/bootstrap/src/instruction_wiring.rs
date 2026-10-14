// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! What the executors of a session need to activate and announce skills,
//! subagents and knowledge documents: the live catalog, the project root that
//! relativises touched paths, and the session's mode.

use std::path::PathBuf;
use std::sync::Arc;

use sven_turn::instructions::{Activator, InstructionCatalog};
use sven_turn::AgentRuntimeContext;

/// Root of the filesystem every effect shares, and the session's activator.
pub(crate) struct InstructionWiring {
    /// Root of the project; also where verification runs.
    pub project_root: Option<PathBuf>,
    activator: Arc<Activator>,
}

impl InstructionWiring {
    /// Wiring over `runtime`'s catalog for a session in `mode`.
    pub(crate) fn new(runtime: &AgentRuntimeContext, mode: &str) -> Self {
        let catalog = InstructionCatalog::new(
            runtime.skills.clone(),
            runtime.agents.clone(),
            runtime.knowledge.clone(),
        );
        Self {
            project_root: runtime.project_root.clone(),
            activator: Arc::new(Activator::new(
                catalog,
                runtime.project_root.clone(),
                mode.to_string(),
            )),
        }
    }

    /// The activator the turn and tool executors share.
    pub(crate) fn activator(&self) -> Arc<Activator> {
        Arc::clone(&self.activator)
    }
}
