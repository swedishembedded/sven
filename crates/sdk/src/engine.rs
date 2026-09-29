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
    tools: Vec<Arc<dyn sven_tool_api::Tool>>,
    toolset: Toolset,
    machines: Option<Arc<sven_machines::ModeRegistry>>,
}

/// The built-in tools an engine's agents get, on top of those registered with
/// [`EngineBuilder::tool`].
///
/// The default is none: an embedded agent can do exactly what its application
/// gave it and nothing else. A preset is an explicit opt-in to sven's own
/// tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Toolset(sven_bootstrap::BuiltinTools);

impl Default for Toolset {
    fn default() -> Self {
        Self::none()
    }
}

impl Toolset {
    /// No built-in tools and no MCP servers. The default.
    #[must_use]
    pub fn none() -> Self {
        Self(sven_bootstrap::BuiltinTools::None)
    }

    /// Read, write and edit files, search, run shell commands, keep a todo
    /// list and ask the user a question.
    #[must_use]
    pub fn coding() -> Self {
        Self(sven_bootstrap::BuiltinTools::Coding)
    }

    /// Read-only: read and search files, keep a todo list, ask a question.
    #[must_use]
    pub fn research() -> Self {
        Self(sven_bootstrap::BuiltinTools::Research)
    }

    pub(crate) fn builtin(self) -> sven_bootstrap::BuiltinTools {
        self.0
    }
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

    /// Creates a fresh agent that runs `mode` and plays `role`.
    ///
    /// The role becomes the agent's system prompt on every turn, which is what
    /// lets it sit in the cacheable prefix rather than being repeated per call.
    #[must_use]
    pub fn agent_with_role(&self, mode: impl Into<String>, role: impl Into<String>) -> Agent {
        Agent::new(self.clone(), AgentState::new(mode).with_role(role))
    }

    /// Creates an agent suited to `method`: the strategy picks the machine,
    /// and the method's role becomes the agent's.
    #[must_use]
    pub fn agent_for<T>(&self, method: &crate::Method<T>) -> Agent
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        let mut state = AgentState::new(mode_for(method.strategy));
        if let Some(role) = method.role.as_deref() {
            state = state.with_role(role);
        }
        Agent::new(self.clone(), state)
    }

    /// Calls a model-driven method without keeping an agent around.
    ///
    /// The lightweight form: no persistent instance state, so nothing
    /// accumulates between calls. Use [`Engine::agent_for`] when successive
    /// calls should build on each other.
    ///
    /// # Errors
    ///
    /// The same failures as [`Agent::call`].
    pub async fn call<I, T>(&self, method: &crate::Method<T>, input: &I) -> Result<T, CallError>
    where
        I: serde::Serialize + ?Sized,
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        self.agent_for(method).call(method, input).await
    }

    /// Resumes a suspended agent against this engine.
    ///
    /// # Errors
    ///
    /// Returns [`CallError::Precondition`] if `state` names a mode this engine
    /// cannot run.
    pub fn resume(&self, state: AgentState) -> Result<Agent, CallError> {
        if !self.knows_mode(state.mode()) {
            return Err(CallError::Precondition(format!(
                "cannot resume agent: this engine cannot run mode {:?}, only {:?}",
                state.mode(),
                self.modes()
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

    pub(crate) fn tools(&self) -> Vec<Arc<dyn sven_tool_api::Tool>> {
        self.tools.clone()
    }

    pub(crate) fn toolset(&self) -> Toolset {
        self.toolset
    }

    pub(crate) fn machines(&self) -> Option<Arc<sven_machines::ModeRegistry>> {
        self.machines.clone()
    }

    /// Whether this engine can run `mode`.
    fn knows_mode(&self, mode: &str) -> bool {
        match &self.machines {
            Some(registry) => registry.get(mode).is_some(),
            None => sven_machines::ModeRegistry::default_registry()
                .get(mode)
                .is_some(),
        }
    }

    /// Every mode this engine can run.
    #[must_use]
    pub fn modes(&self) -> Vec<String> {
        let owned = |registry: &sven_machines::ModeRegistry| -> Vec<String> {
            registry.modes().into_iter().map(str::to_owned).collect()
        };
        match &self.machines {
            Some(registry) => owned(registry),
            None => owned(&sven_machines::ModeRegistry::default_registry()),
        }
    }
}

/// Configures an [`Engine`].
#[derive(Default)]
pub struct EngineBuilder {
    config: Option<Arc<Config>>,
    provider: Option<Arc<dyn ModelProvider>>,
    approvals: Option<ApprovalPolicy>,
    tools: Vec<Arc<dyn sven_tool_api::Tool>>,
    toolset: Toolset,
    machines: Option<sven_machines::ModeRegistry>,
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

    /// Selects the built-in tools every agent on this engine gets. Defaults to
    /// [`Toolset::none`].
    #[must_use]
    pub fn toolset(mut self, toolset: Toolset) -> Self {
        self.toolset = toolset;
        self
    }

    /// Gives every agent on this engine a tool of your own.
    ///
    /// Registered on top of the [`Toolset`], and wins a name collision with
    /// a built-in tool. The tool is permission-gated and audited exactly like a
    /// built-in one: its `kernel_capability` decides which bucket it falls
    /// under, and its `default_policy` whether it needs approval.
    ///
    /// Repeatable.
    #[must_use]
    pub fn tool(mut self, tool: Arc<dyn sven_tool_api::Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Registers a state machine of your own under `mode`.
    ///
    /// The kernel drives it exactly as it drives the built-in machines -
    /// permissions, audit, suspend and resume all apply. Registering an
    /// existing mode name replaces it.
    ///
    /// The machine must implement `all_states()`, or agents running it cannot
    /// be suspended and resumed. See `docs/technical/resumable-agents.md`.
    ///
    /// Repeatable.
    #[must_use]
    pub fn machine(
        mut self,
        mode: impl AsRef<str>,
        factory: sven_machines::mode::MachineFactory,
    ) -> Self {
        self.machines
            .get_or_insert_with(sven_machines::ModeRegistry::default_registry)
            .register(mode.as_ref(), factory);
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
            tools: self.tools,
            toolset: self.toolset,
            machines: self.machines.map(Arc::new),
        })
    }
}

/// The machine that serves a given strategy.
fn mode_for(strategy: crate::Strategy) -> &'static str {
    match strategy {
        crate::Strategy::Predict => "predict",
        crate::Strategy::Investigate => "agent",
    }
}
