// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Native-action escape hatch.
//!
//! The `~20%` of machine logic that cannot be expressed declaratively (SDLC
//! fan-out heuristics, recovery retry counting, child result merging) is
//! registered here as named Rust functions and invoked by name from the DSL.
//!
//! # Contract
//!
//! Native functions must obey the same purity rule as hand-written HSM
//! handlers: they may mutate [`Context`](sven_hsm::context::Context) and return
//! effects, but **must never perform I/O**.  The kernel provides no enforcement
//! mechanism; keep the native surface small and audit it at review time.

use std::collections::HashMap;

use serde_json::Value;
use sven_hsm::{context::Context, effect::Effect, event::Event};

// ─── NativeArgs ──────────────────────────────────────────────────────────────

/// Static parameters declared in the DSL at the call site of a native node or
/// native effect template.
#[derive(Clone, Debug, Default)]
pub struct NativeArgs {
    /// JSON object of static parameters from the DSL (may be `Null`).
    pub params: Value,
}

// ─── NativeOutcome ───────────────────────────────────────────────────────────

/// What a native function tells the graph machine to do.
///
/// Targets are given as node-name strings so native functions are
/// state-type-agnostic and can be reused across graphs.
#[derive(Debug)]
pub enum NativeOutcome {
    /// Consumed the event; stay in the current state; emit these effects.
    Handled(Vec<Effect>),
    /// Transition to the named target node with these effects and rationale.
    Goto {
        target: String,
        effects: Vec<Effect>,
        rationale: String,
    },
    /// Defer to the superstate (equivalent to `Reaction::Super`).
    Bubble,
    /// Not applicable (equivalent to `Reaction::Ignored`).
    Ignore,
}

// ─── NativeFn ────────────────────────────────────────────────────────────────

/// A registered native function.
///
/// The function receives the mutable context (may update facts), the current
/// event, and any static parameters declared at the DSL call site.  It returns
/// a [`NativeOutcome`] telling the graph machine how to react.
///
/// `fn(&mut Context, &Event, &NativeArgs) -> NativeOutcome`
pub type NativeFn = fn(&mut Context, &Event, &NativeArgs) -> NativeOutcome;

// ─── NativeRegistry ──────────────────────────────────────────────────────────

/// A registry of named native functions.
///
/// Populated by `sven-core` with the built-in SDLC and task native fns, and
/// potentially extended by user-supplied graph loaders for custom graphs.
#[derive(Default)]
pub struct NativeRegistry {
    fns: HashMap<String, NativeFn>,
}

impl NativeRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a named function.
    pub fn register(&mut self, name: impl Into<String>, f: NativeFn) {
        self.fns.insert(name.into(), f);
    }

    /// Look up a function by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<NativeFn> {
        self.fns.get(name).copied()
    }

    /// Invoke a named function, returning an error string if not found.
    pub fn call(
        &self,
        fn_name: &str,
        ctx: &mut Context,
        event: &Event,
        args: &NativeArgs,
    ) -> Result<NativeOutcome, String> {
        match self.get(fn_name) {
            Some(f) => Ok(f(ctx, event, args)),
            None => Err(format!("native function not found: {fn_name}")),
        }
    }
}

impl std::fmt::Debug for NativeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NativeRegistry {{ fns: {:?} }}", self.fns.keys().collect::<Vec<_>>())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::context::Context;

    fn noop(ctx: &mut Context, _ev: &Event, _args: &NativeArgs) -> NativeOutcome {
        ctx.set_fact("called", true);
        NativeOutcome::Handled(vec![])
    }

    #[test]
    fn register_and_call() {
        let mut reg = NativeRegistry::new();
        reg.register("noop", noop);

        let mut ctx = Context::new();
        let event = Event::UserCancelled;
        let args = NativeArgs::default();

        let outcome = reg.call("noop", &mut ctx, &event, &args).unwrap();
        assert!(matches!(outcome, NativeOutcome::Handled(_)));
        assert_eq!(ctx.fact("called"), Some(&serde_json::Value::Bool(true)));
    }

    #[test]
    fn unknown_fn_returns_error() {
        let reg = NativeRegistry::new();
        let mut ctx = Context::new();
        let event = Event::UserCancelled;
        let args = NativeArgs::default();
        assert!(reg.call("unknown", &mut ctx, &event, &args).is_err());
    }
}
