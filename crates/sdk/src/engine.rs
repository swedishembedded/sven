// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The process-lifetime resources every agent borrows.

use std::sync::Arc;

use sven_config::Config;
use sven_model::ModelProvider;

use crate::agent::Agent;
use crate::error::CallError;
use crate::state::AgentState;

/// The shared, expensive half of the framework.
///
/// An engine owns what is costly to build and safe to share: the model client
/// and its connection pool, and the configuration every session is derived
/// from. Agents are created against it and borrow those resources rather than
/// each building their own - which is what makes it viable to serve many
/// concurrent agents, or to create one per request, without paying to
/// reconnect every time.
///
/// Cheap to clone: an engine is a bundle of handles, so a service typically
/// builds one at startup and clones it into each request.
#[derive(Clone)]
pub struct Engine {
    config: Arc<Config>,
    provider: Option<Arc<dyn ModelProvider>>,
    approvals: ApprovalPolicy,
}

/// What an agent does when the kernel asks a human to approve something.
///
/// An agent running without a human attached must still answer the gate, or
/// the turn blocks until it is cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Refuse every request. The default: a service with nobody watching
    /// should not silently grant a dangerous capability.
    Deny,
    /// Grant every request, as the headless CI runner does.
    ///
    /// Appropriate only where the workspace is already disposable - a
    /// container, a sandbox, a scratch clone.
    AutoApprove,
}

impl Engine {
    /// Starts configuring an engine.
    #[must_use]
    pub fn builder() -> EngineBuilder {
        EngineBuilder::default()
    }

    /// Creates a fresh agent that runs `mode`, with no history.
    ///
    /// `mode` names a machine in the kernel's mode registry - `"agent"` for
    /// the conversational coding agent, `"sdlc"` for the structured workflow.
    /// An unregistered mode is reported when the agent first runs, not here,
    /// so that constructing an agent is infallible and cheap.
    #[must_use]
    pub fn agent(&self, mode: impl Into<String>) -> Agent {
        Agent::new(self.clone(), AgentState::new(mode))
    }

    /// Resumes a suspended agent against this engine.
    ///
    /// # Errors
    ///
    /// Returns [`CallError::Precondition`] if `state` names a mode this engine
    /// cannot run.
    pub fn resume(&self, state: AgentState) -> Result<Agent, CallError> {
        let registry = sven_machines::ModeRegistry::default_registry();
        if registry.get(state.mode()).is_none() {
            return Err(CallError::Precondition(format!(
                "cannot resume agent: this engine cannot run mode {:?}, only {:?}",
                state.mode(),
                registry.modes()
            )));
        }
        Ok(Agent::new(self.clone(), state))
    }

    pub(crate) fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config)
    }

    pub(crate) fn provider(&self) -> Option<Arc<dyn ModelProvider>> {
        self.provider.clone()
    }

    pub(crate) fn approvals(&self) -> ApprovalPolicy {
        self.approvals
    }
}

/// Configures an [`Engine`].
#[derive(Default)]
pub struct EngineBuilder {
    config: Option<Arc<Config>>,
    provider: Option<Arc<dyn ModelProvider>>,
    approvals: Option<ApprovalPolicy>,
}

impl EngineBuilder {
    /// Uses `config` instead of the default configuration.
    #[must_use]
    pub fn config(mut self, config: impl Into<Arc<Config>>) -> Self {
        self.config = Some(config.into());
        self
    }

    /// Shares one model provider across every agent this engine creates.
    ///
    /// Without this the provider is constructed per session from config, which
    /// means a fresh HTTP client and connection pool each time. Supplying one
    /// here is also the seam a metering or gateway wrapper hangs off: wrap the
    /// provider once and no agent can reach the model unwrapped.
    #[must_use]
    pub fn model_provider(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Sets what agents do at a human-approval gate. Defaults to
    /// [`ApprovalPolicy::Deny`].
    #[must_use]
    pub fn approvals(mut self, policy: ApprovalPolicy) -> Self {
        self.approvals = Some(policy);
        self
    }

    /// Builds the engine.
    ///
    /// Without [`Self::config`] the engine uses [`Config::default`] rather than
    /// reading the user's configuration file - an embedded agent should not
    /// silently inherit whatever is on the host's disk. A caller that does want
    /// the file loads it with `sven_config::load` and passes it in.
    ///
    /// # Errors
    ///
    /// Cannot currently fail. The result type is part of the signature so that
    /// validating an engine's configuration later is not a breaking change.
    pub fn build(self) -> Result<Engine, CallError> {
        let config = match self.config {
            Some(c) => c,
            None => Arc::new(Config::default()),
        };
        Ok(Engine {
            config,
            provider: self.provider,
            approvals: self.approvals.unwrap_or(ApprovalPolicy::Deny),
        })
    }
}
