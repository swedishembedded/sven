//! Mode registry: maps a mode string to a machine factory.
//!
//! Adding a new machine is a one-liner: call [`ModeRegistry::register`] with a
//! factory closure. The registry replaces the hard-coded `AgentMode` enum in
//! `sven-vocab`; all downstream code that picks a machine does so by looking
//! up a plain `&str`.

use std::collections::HashMap;

use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};

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

/// Registry mapping mode strings to machine factories.
///
/// # Example
///
/// ```rust,ignore
/// let reg = ModeRegistry::default_registry();
/// let factory = reg.get("chat").expect("chat mode must exist");
/// let mut machine = factory();
/// ```
pub struct ModeRegistry {
    factories: HashMap<String, MachineFactory>,
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
            factories: HashMap::new(),
        };
        let reactive_factory = || -> MachineFactory {
            Box::new(|| -> Box<dyn ErasedMachine> {
                Box::new(Hsm::new(ReactiveAgentMachine::new()))
            })
        };
        // agent, reactive, and chat all use the proven converse streaming engine.
        // Chat is a natural-language conversation; the user invokes --mode sdlc
        // explicitly when they want the structured SDLC workflow.
        reg.register("agent", reactive_factory());
        reg.register("reactive", reactive_factory());
        reg.register("chat", reactive_factory());
        reg.register(
            "sdlc",
            Box::new(|| -> Box<dyn ErasedMachine> { Box::new(Hsm::new(SdlcMachine::new())) }),
        );
        reg.register(
            "verified-task",
            Box::new(|| -> Box<dyn ErasedMachine> {
                Box::new(Hsm::new(VerifiedTaskMachine::new()))
            }),
        );
        reg.register(
            "predict",
            Box::new(|| -> Box<dyn ErasedMachine> { Box::new(Hsm::new(PredictMachine::new())) }),
        );
        reg
    }

    /// Looks up the factory for `mode`, returning `None` if unregistered.
    pub fn get(&self, mode: &str) -> Option<&MachineFactory> {
        self.factories.get(mode)
    }

    /// Registers (or replaces) a factory for `mode`.
    pub fn register(&mut self, mode: &str, factory: MachineFactory) {
        self.factories.insert(mode.to_string(), factory);
    }

    /// Returns the names of all registered modes.
    pub fn modes(&self) -> Vec<&str> {
        self.factories.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::Context;

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
        let factory = reg
            .get("verified-task")
            .expect("verified-task mode must be registered");
        let mut machine = factory();
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

    #[test]
    fn chat_factory_produces_initializable_machine() {
        let reg = ModeRegistry::default_registry();
        let factory = reg.get("chat").unwrap();
        let mut machine = factory();
        let mut ctx = Context::new();
        // init() returns entry effects without panicking.
        let _ = machine.init(&mut ctx);
    }

    #[test]
    fn sdlc_factory_produces_initializable_machine() {
        let reg = ModeRegistry::default_registry();
        let factory = reg.get("sdlc").unwrap();
        let mut machine = factory();
        let mut ctx = Context::new();
        let _ = machine.init(&mut ctx);
    }

    #[test]
    fn factories_produce_independent_instances() {
        let reg = ModeRegistry::default_registry();
        let factory = reg.get("chat").unwrap();
        let m1 = factory();
        let m2 = factory();
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
        );
        assert!(reg.get("custom").is_some());
    }

    #[test]
    fn get_unknown_mode_returns_none() {
        let reg = ModeRegistry::default_registry();
        assert!(reg.get("nonexistent").is_none());
    }
}
