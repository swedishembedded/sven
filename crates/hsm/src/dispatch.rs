//! The generic Hierarchical State Machine engine.
//!
//! This is a faithful implementation of the two-phase HSM algorithm from
//! `02-hsm-implementation.md` (§4 generic algorithm, §5 entry/exit rules, §6d
//! Rust enum dispatch):
//!
//! * **Phase 1 - find the handler.** Starting at the active leaf, call the
//!   state handler. While it returns [`Reaction::Super`], re-dispatch to the
//!   named parent. This walks the `Super` chain until some state handles the
//!   event (transition / internal) or the root ignores it.
//! * **Phase 2 - execute the transition.** Compute the genuine **Least Common
//!   Ancestor** of the transition source and target by walking both ancestor
//!   chains (no depth-counting shortcut). Fire exit actions bottom-up from the
//!   active leaf to the LCA (exclusive), then the transition action, then entry
//!   actions top-down from below the LCA to the target, then drill into
//!   composites via their `Init` initial transition.
//!
//! Self-transitions are handled by treating the LCA as `superstate(source)`,
//! which makes the source exit and re-enter exactly once (matching the skill's
//! case (A)).
//!
//! Effects are collected in execution order: **exit effects, then the
//! transition-action effects, then entry effects, then any initial-transition
//! effects produced while drilling.**

use std::collections::HashSet;

use crate::audit::AuditRecord;
use crate::context::Context;
use crate::effect::Effect;
use crate::event::{Event, EventKind};
use crate::machine::Machine;
use crate::status::Reaction;

/// The result of dispatching one event.
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    /// State label before the dispatch.
    pub from: String,
    /// State label after the dispatch.
    pub to: String,
    /// The kind of event dispatched.
    pub event: EventKind,
    /// Effects produced, in execution order. Not yet validated or executed.
    pub effects: Vec<Effect>,
    /// `false` only when the event reached the root unhandled (`Ignored`).
    pub handled: bool,
    /// `true` if a transition (including a composite self-transition) occurred.
    pub transitioned: bool,
    /// `true` if the machine is now in a terminal state.
    pub completed: bool,
    /// The audit record for this dispatch.
    pub audit: AuditRecord,
}

/// Drives a [`Machine`] using the HSM algorithm. Owns the machine and tracks the
/// current active leaf state.
#[derive(Debug)]
pub struct Hsm<M: Machine> {
    machine: M,
    state: M::State,
    initialized: bool,
}

impl<M: Machine> Hsm<M> {
    /// Wraps `machine`. The active state is set to the machine's root; call
    /// [`init`](Hsm::init) to fire the initial transitions and enter the first
    /// real leaf state.
    pub fn new(machine: M) -> Self {
        let state = machine.top();
        Self {
            machine,
            state,
            initialized: false,
        }
    }

    /// The current active leaf state.
    pub fn state(&self) -> M::State {
        self.state
    }

    /// The `Debug` label of the current state (used for audit / policy keys).
    pub fn state_label(&self) -> String {
        format!("{:?}", self.state)
    }

    /// Shared access to the wrapped machine.
    pub fn machine(&self) -> &M {
        &self.machine
    }

    /// Mutable access to the wrapped machine.
    pub fn machine_mut(&mut self) -> &mut M {
        &mut self.machine
    }

    /// `true` once [`init`](Hsm::init) has run.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// `true` if the machine is in a terminal state.
    pub fn is_done(&self) -> bool {
        self.machine.is_terminal(self.state)
    }

    /// Fires the topmost initial transition: enters from the root down to
    /// [`Machine::initial`], then drills into composites. Returns the entry
    /// effects in execution order. Idempotent-guarded: a second call is a
    /// no-op returning no effects.
    pub fn init(&mut self, ctx: &mut Context) -> Vec<Effect> {
        if self.initialized {
            return Vec::new();
        }
        let top = self.machine.top();
        let target = self.machine.initial();

        let mut effects = Vec::new();
        for st in self.entry_path(top, target) {
            effects.extend(self.fire_entry(st, ctx));
        }
        self.state = target;
        self.drill_into_composites(ctx, &mut effects);
        self.initialized = true;
        effects
    }

    /// Dispatches one ordinary event, returning the full outcome (effects +
    /// audit). Lifecycle signals must not be passed here; they are generated
    /// internally by the engine.
    pub fn dispatch(&mut self, event: &Event, ctx: &mut Context) -> DispatchOutcome {
        debug_assert!(
            !event.is_lifecycle(),
            "lifecycle signals (entry/exit/init) must not be dispatched as ordinary events"
        );

        let from = self.state_label();
        let event_kind = event.kind();
        let active = self.state;

        // --- Phase 1: walk the Super chain to find the handling state. ---
        let mut source = active;
        let mut reaction = self.machine.dispatch_state(source, event, ctx);
        while let Reaction::Super(parent) = reaction {
            debug_assert_ne!(
                parent, source,
                "superstate(state) must differ from state except at the root"
            );
            source = parent;
            reaction = self.machine.dispatch_state(source, event, ctx);
        }

        // --- Phase 2: act on the reaction. ---
        let mut outcome = match reaction {
            Reaction::Transition {
                target,
                effects: tran_effects,
                rationale,
            } => {
                let effects = self.execute_transition(active, source, target, tran_effects, ctx);
                // (effects already in order: exits, transition action, entries, init)
                let to = self.state_label();
                let audit = AuditRecord::transition(
                    from.clone(),
                    to.clone(),
                    event_kind,
                    &effects,
                    Some(rationale).filter(|r| !r.is_empty()),
                );
                DispatchOutcome {
                    from,
                    to,
                    event: event_kind,
                    effects,
                    handled: true,
                    transitioned: true,
                    completed: self.is_done(),
                    audit,
                }
            }
            Reaction::Handled(effects) => {
                let to = self.state_label();
                let audit = AuditRecord::internal(from.clone(), event_kind, &effects);
                DispatchOutcome {
                    from,
                    to,
                    event: event_kind,
                    effects,
                    handled: true,
                    transitioned: false,
                    completed: self.is_done(),
                    audit,
                }
            }
            Reaction::Ignored => {
                let to = self.state_label();
                let audit = AuditRecord::ignored(from.clone(), event_kind);
                DispatchOutcome {
                    from,
                    to,
                    event: event_kind,
                    effects: Vec::new(),
                    handled: false,
                    transitioned: false,
                    completed: self.is_done(),
                    audit,
                }
            }
            // Unreachable: the phase-1 loop only exits on a non-`Super` reaction.
            Reaction::Super(_) => unreachable!("phase 1 loop exits before a Super reaction"),
        };

        // Every dispatch appends exactly one audit record (event-sourcing spine),
        // attributed to the session principal when one is set.
        outcome.audit.stamp_principal(ctx.principal.as_ref());
        ctx.audit.push(outcome.audit.clone());
        outcome
    }

    /// Executes a transition `source -> target` while the active leaf is
    /// `active` (which may be a descendant of `source` for inherited
    /// transitions). Returns the effects in execution order.
    fn execute_transition(
        &mut self,
        active: M::State,
        source: M::State,
        target: M::State,
        tran_effects: Vec<Effect>,
        ctx: &mut Context,
    ) -> Vec<Effect> {
        // The genuine LCA, except for self-transitions where we force a full
        // exit/enter cycle by treating the parent of the source as the LCA.
        let lca = if source == target {
            self.machine.superstate(source)
        } else {
            self.find_lca(source, target)
        };

        let mut effects = Vec::new();

        // Exit actions, bottom-up from the active leaf to the LCA (exclusive).
        let mut cur = active;
        while cur != lca {
            effects.extend(self.fire_exit(cur, ctx));
            let parent = self.machine.superstate(cur);
            debug_assert_ne!(parent, cur, "exited past the root without reaching the LCA");
            cur = parent;
        }

        // Transition action, after all exits and before any entries (§5).
        effects.extend(tran_effects);

        // Entry actions, top-down from below the LCA to the target.
        for st in self.entry_path(lca, target) {
            effects.extend(self.fire_entry(st, ctx));
        }
        self.state = target;

        // Drill into composites via their initial transitions.
        self.drill_into_composites(ctx, &mut effects);
        effects
    }

    /// Repeatedly fires the current state's `Init` transition (if any), entering
    /// the default substate path, until a leaf with no initial transition is
    /// reached. Appends entry/init-action effects to `effects`.
    fn drill_into_composites(&mut self, ctx: &mut Context, effects: &mut Vec<Effect>) {
        loop {
            let r = self.machine.dispatch_state(self.state, &Event::init(), ctx);
            match r {
                Reaction::Transition {
                    target: sub,
                    effects: init_effects,
                    ..
                } => {
                    effects.extend(init_effects);
                    for st in self.entry_path(self.state, sub) {
                        effects.extend(self.fire_entry(st, ctx));
                    }
                    self.state = sub;
                }
                _ => break,
            }
        }
    }

    /// Fires a state's entry action. Entry handlers may emit effects but must
    /// never transition (skill Unbreakable Rule 2).
    fn fire_entry(&mut self, state: M::State, ctx: &mut Context) -> Vec<Effect> {
        let r = self.machine.dispatch_state(state, &Event::entry(), ctx);
        Self::lifecycle_effects(r, "entry")
    }

    /// Fires a state's exit action. Same no-transition rule as entry.
    fn fire_exit(&mut self, state: M::State, ctx: &mut Context) -> Vec<Effect> {
        let r = self.machine.dispatch_state(state, &Event::exit(), ctx);
        Self::lifecycle_effects(r, "exit")
    }

    /// Extracts effects from an entry/exit reaction, asserting it is not a
    /// transition. `Super`/`Ignored` mean "no action for this signal here".
    fn lifecycle_effects(reaction: Reaction<M::State>, which: &str) -> Vec<Effect> {
        match reaction {
            Reaction::Handled(effects) => effects,
            Reaction::Ignored | Reaction::Super(_) => Vec::new(),
            Reaction::Transition { .. } => {
                debug_assert!(
                    false,
                    "{which} handler must not transition (Unbreakable Rule 2)"
                );
                Vec::new()
            }
        }
    }

    /// Builds the entry path from just below `from_exclusive` down to
    /// `to_inclusive`, ordered top-down (ancestors first). `from_exclusive` must
    /// be an ancestor of `to_inclusive`; if they are equal the path is empty.
    fn entry_path(&self, from_exclusive: M::State, to_inclusive: M::State) -> Vec<M::State> {
        let mut path = Vec::new();
        let mut cur = to_inclusive;
        while cur != from_exclusive {
            path.push(cur);
            let parent = self.machine.superstate(cur);
            if parent == cur {
                // Reached the root without hitting `from_exclusive`: `from` was
                // not an ancestor of `to`. Well-formed machines never hit this.
                break;
            }
            cur = parent;
        }
        path.reverse();
        path
    }

    /// Computes the genuine Least Common Ancestor of `s` and `t` by collecting
    /// `t`'s ancestor set and walking `s` upward until the first common
    /// ancestor. Returns the root if the two share only the root.
    fn find_lca(&self, s: M::State, t: M::State) -> M::State {
        let mut t_ancestors: HashSet<M::State> = HashSet::new();
        let mut cur = t;
        loop {
            t_ancestors.insert(cur);
            let parent = self.machine.superstate(cur);
            if parent == cur {
                break;
            }
            cur = parent;
        }

        let mut cur = s;
        loop {
            if t_ancestors.contains(&cur) {
                return cur;
            }
            let parent = self.machine.superstate(cur);
            if parent == cur {
                return cur; // root; shared by everything
            }
            cur = parent;
        }
    }
}
