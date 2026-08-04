//! The trait every concrete state machine implements.
//!
//! A `Machine` describes a state hierarchy purely in terms of:
//!
//! * its **states** (the associated [`Machine::State`], opaque to the kernel);
//! * the **superstate** of each state (the tree structure);
//! * a single **handler** ([`Machine::dispatch_state`]) that, given a state and
//!   an event, returns a [`Reaction`].
//!
//! The kernel's [`Hsm`](crate::dispatch::Hsm) drives any `Machine` generically
//! using the full HSM algorithm. Handlers must follow the skill's two
//! unbreakable rules:
//!
//! 1. Handlers are **pure**: they may mutate [`Context`] and *return* effects,
//!    but never perform I/O.
//! 2. **Entry/exit handlers never transition** (the engine enforces this with a
//!    `debug_assert!`). The `Init` signal is the *only* lifecycle signal allowed
//!    to return [`Reaction::Transition`], because firing a composite's initial
//!    transition is its whole purpose.

use std::fmt::Debug;
use std::hash::Hash;

use crate::context::Context;
use crate::event::Event;
use crate::ids::MachineId;
use crate::status::Reaction;

/// A hierarchical state machine definition.
///
/// # State hierarchy contract
///
/// * There is exactly one **root** state `r` with `superstate(r) == r` (a
///   fixpoint). Return it from [`top`](Machine::top).
/// * Every other state must reach the root by repeatedly applying
///   [`superstate`](Machine::superstate) (no cycles, no orphans).
/// * [`initial`](Machine::initial) returns the target of the root's initial
///   transition (the first real state entered at startup). It must be a proper
///   descendant of `top`.
pub trait Machine {
    /// The machine's state identifier. Opaque to the kernel; it only needs to
    /// compare, hash, copy and debug-print states.
    type State: Copy + Eq + Hash + Debug + 'static;

    /// This instance's identity (used in audit records and submachine wiring).
    fn id(&self) -> MachineId;

    /// The root state, for which `superstate(top) == top`.
    fn top(&self) -> Self::State;

    /// The target of the root's initial transition (entered at startup).
    fn initial(&self) -> Self::State;

    /// The parent of `state`. Must return `top()` (or a fixpoint) for the root.
    fn superstate(&self, state: Self::State) -> Self::State;

    /// Handle `event` while in `state`.
    ///
    /// Called both for ordinary events and for the reserved lifecycle signals
    /// ([`Event::entry`], [`Event::exit`], [`Event::init`]). For ordinary events
    /// a handler that does not deal with the event should return
    /// [`Reaction::Super`] naming its parent, so the engine can propagate it.
    fn dispatch_state(
        &mut self,
        state: Self::State,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<Self::State>;

    /// `true` if `state` is a terminal/Done state. The runtime and submachine
    /// host use this to detect completion. Defaults to `false`.
    fn is_terminal(&self, _state: Self::State) -> bool {
        false
    }

    /// All states of the machine, used by coverage assertions in tests.
    /// Defaults to empty (coverage tooling then simply has nothing to check).
    fn all_states(&self) -> Vec<Self::State> {
        Vec::new()
    }
}
