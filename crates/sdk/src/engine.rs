// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The process-lifetime resources every agent borrows.

use std::sync::Arc;

use sven_bootstrap::Config;
use sven_model::ModelProvider;

use crate::agent::Agent;
use crate::error::CallError;
use crate::gates::GateHandler;
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
    human: Option<GateHandler>,
    park_questions: bool,
    park_approvals: bool,
    tools: Vec<Arc<dyn sven_tool_api::Tool>>,
    toolset: Toolset,
    machines: Option<Arc<sven_machines::ModeRegistry>>,
    paths: sven_tool_api::PathScope,
    /// The most a harness run on this engine may do, whatever its document
    /// declares; `None` leaves the document's own declaration the whole cap.
    host_capabilities: Option<Arc<[sven_hsm::ToolCapability]>>,
    /// The named harnesses this engine's sessions resolve `delegate` and
    /// `parallel` children against, and that [`Engine::load_harness`]
    /// registers into.
    pub(crate) harnesses: Arc<sven_harness_runtime::HarnessRegistry>,
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

    /// Read, write, edit and attach files, search, run shell commands, fetch
    /// and search the web, keep a todo list and a memory file, load skills,
    /// switch mode or model, delegate to sub-agents (`task`) and ask the user
    /// a question. Attaching images and audio, and transcribing speech, need
    /// the `coding` cargo feature (on by default).
    #[must_use]
    pub fn coding() -> Self {
        Self(sven_bootstrap::BuiltinTools::Coding)
    }

    /// Read-only: read and search files, fetch and search the web, keep a
    /// todo list and a memory file, load skills, switch mode or model,
    /// delegate to read-only sub-agents and ask a question.
    #[must_use]
    pub fn research() -> Self {
        Self(sven_bootstrap::BuiltinTools::Research)
    }

    pub(crate) fn builtin(self) -> sven_bootstrap::BuiltinTools {
        self.0
    }
}

/// Whether an agent's tool calls wait for a person's approval.
///
/// Approval never widens what an agent may do - its mode decides that - it
/// only decides whether an allowed call is put to a person first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Every call the agent's mode allows runs without asking anyone, and a
    /// decision a machine puts to a person (an SDLC `need_approval`) is
    /// approved. The default.
    #[default]
    Auto,
    /// Every call that is not read-only - the agent's and its children's - is
    /// put to the engine's human-gate handler ([`EngineBuilder::human_gates`])
    /// as a [`HumanGate::Approval`](crate::HumanGate::Approval) carrying the
    /// tool and its arguments, and runs only once approved; each call is
    /// asked about on its own. An engine under manual approval without a
    /// handler does not build.
    Manual,
}

impl From<ApprovalPolicy> for sven_vocab::ApprovalMode {
    fn from(policy: ApprovalPolicy) -> Self {
        match policy {
            ApprovalPolicy::Auto => Self::Auto,
            ApprovalPolicy::Manual => Self::Manual,
        }
    }
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

    /// [`Engine::call`] within `bounds`, which cover the whole call,
    /// correction attempts included.
    ///
    /// # Errors
    ///
    /// The same failures as [`Agent::call_with`].
    pub async fn call_with<I, T>(
        &self,
        method: &crate::Method<T>,
        input: &I,
        bounds: crate::RunOptions,
    ) -> Result<T, CallError>
    where
        I: serde::Serialize + ?Sized,
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        self.agent_for(method)
            .call_with(method, input, bounds)
            .await
    }

    /// Resumes a suspended agent against this engine.
    ///
    /// # Errors
    ///
    /// Returns [`CallError::Precondition`] if `state` names a mode this engine
    /// cannot run, or was written by a newer format than this build reads.
    pub fn resume(&self, state: AgentState) -> Result<Agent, CallError> {
        if state.version() > AgentState::CURRENT_VERSION {
            return Err(CallError::Precondition(format!(
                "cannot resume agent: its state is version {}, newer than the version {} \
                 this build reads",
                state.version(),
                AgentState::CURRENT_VERSION
            )));
        }
        if !self.knows_mode(state.mode()) {
            return Err(CallError::Precondition(format!(
                "cannot resume agent: this engine cannot run mode {:?}, only {:?}",
                state.mode(),
                self.modes()
            )));
        }
        Ok(Agent::new(self.clone(), state.migrated()))
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

    pub(crate) fn human(&self) -> Option<GateHandler> {
        self.human.clone()
    }

    pub(crate) fn parks_questions(&self) -> bool {
        self.park_questions
    }

    pub(crate) fn parks_approvals(&self) -> bool {
        self.park_approvals
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

    /// The directory the agents work in, canonical; `None` without
    /// [`EngineBuilder::project_root`].
    #[must_use]
    pub fn project_root(&self) -> Option<&std::path::Path> {
        self.paths.root()
    }

    /// The host's capability ceiling for harness runs, when one is set.
    pub(crate) fn host_policy(&self) -> Option<sven_hsm::PermissionPolicy> {
        self.host_capabilities.as_ref().map(|caps| {
            sven_hsm::PermissionPolicy::builder()
                .allow_globally(caps.iter().copied())
                .build()
        })
    }

    pub(crate) fn paths(&self) -> sven_tool_api::PathScope {
        self.paths.clone()
    }

    /// Whether this engine can run `mode`.
    fn knows_mode(&self, mode: &str) -> bool {
        match &self.machines {
            Some(registry) => registry.get(mode).is_some(),
            None => sven_bootstrap::mode_registry().get(mode).is_some(),
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
            None => owned(&sven_bootstrap::mode_registry()),
        }
    }
}

/// Configures an [`Engine`].
#[derive(Default)]
pub struct EngineBuilder {
    config: Option<Arc<Config>>,
    provider: Option<Arc<dyn ModelProvider>>,
    approvals: ApprovalPolicy,
    human: Option<GateHandler>,
    park_questions: bool,
    park_approvals: bool,
    tools: Vec<Arc<dyn sven_tool_api::Tool>>,
    toolset: Toolset,
    machines: Option<sven_machines::ModeRegistry>,
    project_root: Option<std::path::PathBuf>,
    host_capabilities: Option<Vec<sven_hsm::ToolCapability>>,
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
    /// under: the agent's mode decides whether that bucket may run, and
    /// [`ApprovalPolicy::Manual`] whether a call of it waits for a person
    /// (every bucket but reads does). `default_policy` applies only where a
    /// permission requester fronts the registry (ACP, MCP).
    ///
    /// Repeatable.
    #[must_use]
    pub fn tool(mut self, tool: Arc<dyn sven_tool_api::Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Registers a state machine of your own under `mode`, running under
    /// `policy`.
    ///
    /// The kernel drives it exactly as it drives the built-in machines -
    /// permissions, audit, suspend and resume all apply, and every session in
    /// the mode is gated on `policy` (then narrowed by the session's approval
    /// mode). A policy is part of what a mode is: without one the kernel
    /// would have to guess what the machine's states may do. Registering an
    /// existing mode name replaces it.
    ///
    /// The machine must implement `all_states()`, or agents running it cannot
    /// be suspended and resumed. See `docs/internals/resumable-agents.md`.
    ///
    /// Repeatable.
    #[must_use]
    pub fn machine(
        mut self,
        mode: impl AsRef<str>,
        factory: sven_machines::mode::MachineFactory,
        policy: sven_hsm::PermissionPolicy,
    ) -> Self {
        self.machines
            .get_or_insert_with(sven_bootstrap::mode_registry)
            .register(mode.as_ref(), factory, policy);
        self
    }

    /// Makes `root` the directory every agent on this engine works in.
    ///
    /// The built-in file tools resolve relative paths against it and refuse
    /// any path that resolves outside it, symlinks included; the `shell`
    /// tool starts there and refuses a `workdir` outside it; the audit log
    /// is written to `<root>/.sven/audit.jsonl`; and skills, sub-agent
    /// personas, project knowledge and the project context file are
    /// discovered from it as the CLI discovers them from its project.
    ///
    /// A shell command can still reach outside the root - a shell is not a
    /// sandbox. The root confines the file tools and where commands start,
    /// not what a command does; an application that needs containment runs
    /// the agent in a sandbox. Tools registered with [`Self::tool`] are the
    /// application's own and are not confined.
    ///
    /// Without a root, paths resolve against the process working directory
    /// and the audit log is written beneath it.
    #[must_use]
    pub fn project_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.project_root = Some(root.into());
        self
    }

    /// Caps what every harness run on this engine may do at `capabilities`.
    ///
    /// A harness document declares the capabilities it needs; the host
    /// decides how many of them it is willing to grant. A run holds a
    /// capability only when its document declares it **and** this ceiling
    /// lists it - a ceiling never grants what a document did not ask for,
    /// and a delegated child can hold no more than its parent does. An
    /// empty list grants nothing. Without a ceiling the document's
    /// declaration is the whole cap. Agents driven by a mode (not a
    /// harness) are not affected: their cap is the mode's policy.
    #[must_use]
    pub fn host_capabilities(
        mut self,
        capabilities: impl IntoIterator<Item = sven_hsm::ToolCapability>,
    ) -> Self {
        self.host_capabilities = Some(capabilities.into_iter().collect());
        self
    }

    /// Sets whether agents' tool calls wait for a person's approval.
    /// Defaults to [`ApprovalPolicy::Auto`]; [`ApprovalPolicy::Manual`] needs
    /// a [`Self::human_gates`] handler.
    #[must_use]
    pub fn approvals(mut self, policy: ApprovalPolicy) -> Self {
        self.approvals = policy;
        self
    }

    /// Hands every question an agent asks - the model's `ask_question`, a
    /// machine's own question - and, under [`ApprovalPolicy::Manual`], every
    /// approval request to `answer`, as a [`HumanGate`](crate::HumanGate).
    ///
    /// The handler owns replying through the channel each gate carries: now,
    /// or later, after asking someone. A gate dropped without a reply never
    /// leaves the turn waiting: a question is answered with
    /// [`NO_USER_ANSWER`](crate::tool::NO_USER_ANSWER), and an approval is
    /// denied with the reason `handler_dropped`.
    ///
    /// This is the handler of every agent of the engine; an agent with a
    /// handler of its own ([`Agent::with_human_gates`](crate::Agent::with_human_gates))
    /// uses that instead.
    ///
    /// Without a handler nobody is there to ask: a question is answered at
    /// once with [`NO_USER_ANSWER`](crate::tool::NO_USER_ANSWER) (unless
    /// [`Self::park_questions`]), and the model carries on stating its
    /// assumption.
    #[must_use]
    pub fn human_gates(
        mut self,
        answer: impl Fn(crate::HumanGate) + Send + Sync + 'static,
    ) -> Self {
        self.human = Some(Arc::new(answer));
        self
    }

    /// Parks the run on a question the model asks with `ask_question`
    /// instead of answering it: the run ends [`RunConclusion::Waiting`](crate::RunConclusion::Waiting)
    /// with the question in [`RunOutcome::question`](crate::RunOutcome::question),
    /// and [`Agent::answer`](crate::Agent::answer) continues it, also after a
    /// suspend and resume in another process. The run returns at once; it
    /// never waits for the answer.
    #[must_use]
    pub fn park_questions(mut self) -> Self {
        self.park_questions = true;
        self
    }

    /// Parks an approval instead of putting it to a handler, under
    /// [`ApprovalPolicy::Manual`]: the run ends
    /// [`RunConclusion::Waiting`](crate::RunConclusion::Waiting) with the
    /// approval in [`RunOutcome::approval`](crate::RunOutcome::approval), and
    /// [`Agent::decide`](crate::Agent::decide) continues it, also after a
    /// suspend and resume in another process. The run returns at once; it
    /// never waits for the decision, and no [`Self::human_gates`] handler is
    /// needed for it.
    ///
    /// Only the agent's own calls are parked. A call made by a sub-agent
    /// started with the `task` tool is still put to the handler (or refused
    /// without one), because the sub-agent does not outlive the run.
    #[must_use]
    pub fn park_approvals(mut self) -> Self {
        self.park_approvals = true;
        self
    }

    /// Builds the engine.
    ///
    /// Without [`Self::config`] the engine uses [`Config::default`] rather than
    /// reading the user's configuration file - an embedded agent should not
    /// silently inherit whatever is on the host's disk. A caller that does want
    /// the file loads it with [`config::load`](crate::config::load) and passes it in.
    ///
    /// # Errors
    ///
    /// [`CallError::Precondition`] when a [`Self::project_root`] does not
    /// exist or is not a directory, or under [`ApprovalPolicy::Manual`]
    /// without a [`Self::human_gates`] handler or [`Self::park_approvals`] -
    /// nobody could approve a call, and approving it on their behalf is what
    /// manual approval rules out - and when [`Self::park_approvals`] is set
    /// under any other policy, where nothing is ever asked.
    pub fn build(self) -> Result<Engine, CallError> {
        if self.park_approvals && self.approvals != ApprovalPolicy::Manual {
            return Err(CallError::Precondition(
                "park_approvals parks what manual approval asks: use ApprovalPolicy::Manual".into(),
            ));
        }
        if self.approvals == ApprovalPolicy::Manual && self.human.is_none() && !self.park_approvals
        {
            return Err(CallError::Precondition(
                "manual approval needs a person to approve each call: give the engine a \
                 human_gates handler, park_approvals, or use ApprovalPolicy::Auto"
                    .into(),
            ));
        }
        let config = match self.config {
            Some(c) => c,
            None => Arc::new(Config::default()),
        };
        let paths = match &self.project_root {
            Some(root) => sven_tool_api::PathScope::confined(root)
                .map_err(|e| CallError::Precondition(e.to_string()))?,
            None => sven_tool_api::PathScope::unconfined(),
        };
        Ok(Engine {
            config,
            provider: self.provider,
            approvals: self.approvals,
            human: self.human,
            park_questions: self.park_questions,
            park_approvals: self.park_approvals,
            tools: self.tools,
            toolset: self.toolset,
            machines: self.machines.map(Arc::new),
            paths,
            host_capabilities: self.host_capabilities.map(Into::into),
            harnesses: Arc::new(sven_harness_runtime::HarnessRegistry::new()),
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
