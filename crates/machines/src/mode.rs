//! Mode registry: maps a mode string to a policy-bearing machine registration.
//!
//! Adding a new machine is a one-liner: call [`ModeRegistry::register`] with a
//! [`MachineRegistration`] naming its factory and the [`PermissionPolicy`] it
//! runs under. The policy travels with the registration because a mode's
//! permissions are facts about the machine, known where the machine is
//! registered — not something the session assembler may guess. A mode the
//! registry does not hold has no policy and cannot run: the assembler reports
//! it instead of silently granting a default.
//!
//! The registry replaces the hard-coded `AgentMode` enum in `sven-vocab`; all
//! downstream code that picks a machine does so by looking up a plain `&str`.

use std::collections::HashMap;

use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine, PermissionPolicy};

use crate::machines::{
    predict::PredictMachine, reactive_agent::ReactiveAgentMachine, sdlc::SdlcMachine,
    verified_task::VerifiedTaskMachine,
};

/// The conversation thread a mode's machine reads.
///
/// A session seeds its system message and prior history into one thread before
/// the machine starts. Seeding the wrong one is silent: the machine simply
/// begins with no context and no error, so this mapping is kept next to the
/// registry that decides which machine a mode builds rather than left to each
/// caller to remember.
///
/// Modes whose machines use per-phase threads of their own (`sdlc`) ignore the
/// seed entirely, and fall through to the default.
#[must_use]
pub fn primary_thread(mode: &str) -> &'static str {
    match mode {
        "predict" => crate::machines::predict::PREDICT_THREAD,
        _ => crate::machines::reactive_agent::CHAT_THREAD,
    }
}

/// A factory that creates a type-erased running machine.
pub type MachineFactory = Box<dyn Fn() -> Box<dyn ErasedMachine> + Send + Sync>;

/// One registered mode: how to build its machine, and the policy it runs
/// under.
///
/// The policy is part of the registration, not a guess the session assembler
/// makes from the mode name: whoever registers a machine knows what its
/// states do, and the assembler applies only what it itself knows (the
/// session's approval mode and host-side narrowing).
pub struct MachineRegistration {
    /// Builds a fresh instance of the machine.
    pub factory: MachineFactory,
    /// The permission policy every session running this mode enforces, before
    /// the session's approval mode narrows it.
    pub policy: PermissionPolicy,
}

/// Registry mapping mode strings to policy-bearing machine registrations.
///
/// # Example
///
/// ```rust,ignore
/// let reg = ModeRegistry::default_registry();
/// let registration = reg.get("chat").expect("chat mode must exist");
/// let mut machine = (registration.factory)();
/// ```
pub struct ModeRegistry {
    registrations: HashMap<String, MachineRegistration>,
}

impl Default for ModeRegistry {
    fn default() -> Self {
        Self::default_registry()
    }
}

impl ModeRegistry {
    /// Builds the registry pre-loaded with the built-in machines that need no
    /// particular tool:
    /// - `"agent"` / `"reactive"` / `"chat"` → [`ReactiveAgentMachine`]
    ///   (streaming, native-tool-calling agent — ChatGPT-style)
    /// - `"sdlc"` → [`SdlcMachine`]
    ///   (multi-phase software-development lifecycle with in-state tool loops)
    /// - `"verified-task"` → [`VerifiedTaskMachine`]
    ///   (freeze-before-attempt, externally-verified single task with retry)
    /// - `"predict"` → [`PredictMachine`]
    ///
    /// A machine that drives one specific tool is not here: its mode is
    /// registered by the assembly that compiles that tool in, so no registry
    /// offers a mode whose every step would call a tool that does not exist.
    /// [`UiTestMachine`](crate::UiTestMachine) (`"ui-test"`, the `android`
    /// tool) is one.
    pub fn default_registry() -> Self {
        let mut reg = Self {
            registrations: HashMap::new(),
        };
        let reactive_factory = || -> MachineFactory {
            Box::new(|| -> Box<dyn ErasedMachine> {
                Box::new(Hsm::new(ReactiveAgentMachine::new()))
            })
        };
        // agent, reactive, and chat all use the proven converse streaming engine.
        // Chat is a natural-language conversation; the user invokes --mode sdlc
        // explicitly when they want the structured SDLC workflow.
        let reactive = ReactiveAgentMachine::permission_policy();
        reg.register("agent", reactive_factory(), reactive.clone());
        reg.register("reactive", reactive_factory(), reactive.clone());
        reg.register("chat", reactive_factory(), reactive);
        reg.register(
            "sdlc",
            Box::new(|| -> Box<dyn ErasedMachine> { Box::new(Hsm::new(SdlcMachine::new())) }),
            SdlcMachine::permission_policy(),
        );
        reg.register(
            "verified-task",
            Box::new(|| -> Box<dyn ErasedMachine> {
                Box::new(Hsm::new(VerifiedTaskMachine::new()))
            }),
            VerifiedTaskMachine::permission_policy(),
        );
        // A predict session only ever talks to the model: its machine emits
        // `CallLlm` and nothing else, and no capability-gated effect, so the
        // policy that describes it allows nothing. What the session may read
        // or write is the host's decision, layered on at assembly.
        reg.register(
            "predict",
            Box::new(|| -> Box<dyn ErasedMachine> { Box::new(Hsm::new(PredictMachine::new())) }),
            PermissionPolicy::default(),
        );
        reg
    }

    /// Looks up the registration for `mode`, returning `None` if unregistered.
    pub fn get(&self, mode: &str) -> Option<&MachineRegistration> {
        self.registrations.get(mode)
    }

    /// Registers (or replaces) `mode` with `registration`: a factory plus the
    /// policy every session running the mode enforces.
    pub fn register(&mut self, mode: &str, factory: MachineFactory, policy: PermissionPolicy) {
        self.registrations
            .insert(mode.to_string(), MachineRegistration { factory, policy });
    }

    /// Returns the names of all registered modes.
    pub fn modes(&self) -> Vec<&str> {
        self.registrations.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::{Context, ToolCapability};

    #[test]
    fn default_registry_has_chat_and_sdlc() {
        let reg = ModeRegistry::default_registry();
        assert!(reg.get("chat").is_some(), "chat mode must be registered");
        assert!(reg.get("sdlc").is_some(), "sdlc mode must be registered");
        assert!(reg.get("unknown").is_none());
    }

    #[test]
    fn default_registry_has_verified_task() {
        let reg = ModeRegistry::default_registry();
        let registration = reg
            .get("verified-task")
            .expect("verified-task mode must be registered");
        let mut machine = (registration.factory)();
        let mut ctx = Context::new();
        let _ = machine.init(&mut ctx);
    }

    /// `ui-test` drives the `android` tool, which this crate cannot know is
    /// compiled in; the assembly that compiles it in registers the mode.
    #[test]
    fn default_registry_offers_no_mode_tied_to_one_tool() {
        let reg = ModeRegistry::default_registry();
        assert!(reg.get("ui-test").is_none());
    }

    #[test]
    fn default_registry_has_reactive_agent() {
        let reg = ModeRegistry::default_registry();
        assert!(reg.get("agent").is_some(), "agent mode must be registered");
        assert!(
            reg.get("reactive").is_some(),
            "reactive alias must be registered"
        );
    }

    /// The policy travels with the registration: every built-in mode's
    /// registry entry states exactly what its machine's own policy says, so
    /// the session assembler never guesses a mode's permissions from its
    /// name.
    #[test]
    fn every_built_in_mode_registers_its_own_machines_policy() {
        let reg = ModeRegistry::default_registry();
        let cases: Vec<(&str, PermissionPolicy)> = vec![
            ("agent", ReactiveAgentMachine::permission_policy()),
            ("reactive", ReactiveAgentMachine::permission_policy()),
            ("chat", ReactiveAgentMachine::permission_policy()),
            ("sdlc", SdlcMachine::permission_policy()),
            ("verified-task", VerifiedTaskMachine::permission_policy()),
            // Predict only talks to the model: no capability-gated effect, so
            // its honest policy allows nothing.
            ("predict", PermissionPolicy::default()),
        ];
        for (mode, policy) in cases {
            let registration = reg.get(mode).expect("built-in mode is registered");
            assert_eq!(
                registration.policy, policy,
                "mode {mode:?} must carry its machine's own policy"
            );
        }
    }

    /// The reactive machine's registered policy grants the ordinary agent
    /// set; asserting one representative capability keeps this honest without
    /// restating the whole policy.
    #[test]
    fn a_registered_policy_is_a_real_allow_set_not_a_placeholder() {
        let reg = ModeRegistry::default_registry();
        let agent = reg.get("agent").expect("agent is registered");
        assert!(agent.policy.allows_in_every_state(ToolCapability::ReadFile));
        let sdlc = reg.get("sdlc").expect("sdlc is registered");
        assert!(
            !sdlc.policy.allows_in_every_state(ToolCapability::WriteFile),
            "sdlc writes only in Execution, never in every state"
        );
    }

    #[test]
    fn chat_factory_produces_initializable_machine() {
        let reg = ModeRegistry::default_registry();
        let registration = reg.get("chat").unwrap();
        let mut machine = (registration.factory)();
        let mut ctx = Context::new();
        // init() returns entry effects without panicking.
        let _ = machine.init(&mut ctx);
    }

    #[test]
    fn sdlc_factory_produces_initializable_machine() {
        let reg = ModeRegistry::default_registry();
        let registration = reg.get("sdlc").unwrap();
        let mut machine = (registration.factory)();
        let mut ctx = Context::new();
        let _ = machine.init(&mut ctx);
    }

    #[test]
    fn factories_produce_independent_instances() {
        let reg = ModeRegistry::default_registry();
        let registration = reg.get("chat").unwrap();
        let m1 = (registration.factory)();
        let m2 = (registration.factory)();
        // Each call produces a distinct machine (different MachineId).
        assert_ne!(m1.id(), m2.id());
    }

    #[test]
    fn register_custom_mode_is_retrievable() {
        let mut reg = ModeRegistry::default_registry();
        reg.register(
            "custom",
            Box::new(|| -> Box<dyn ErasedMachine> {
                Box::new(Hsm::new(ReactiveAgentMachine::new()))
            }),
            ReactiveAgentMachine::permission_policy(),
        );
        let registration = reg.get("custom").expect("registered mode is stored");
        assert_eq!(
            registration.policy,
            ReactiveAgentMachine::permission_policy(),
            "the policy given at registration is the policy stored"
        );
    }

    #[test]
    fn get_unknown_mode_returns_none() {
        let reg = ModeRegistry::default_registry();
        assert!(reg.get("nonexistent").is_none());
    }
}
