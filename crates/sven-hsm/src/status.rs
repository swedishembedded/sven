//! The reaction a state handler returns.
//!
//! This unifies the state-machine skill's four-way `Status` (`Handled`,
//! `Ignored`, `Tran`, `Super`) with the effect-returning design: instead of a
//! handler performing side effects, it *returns* the effects it wants the
//! runtime to perform.
//!
//! The handler's contract:
//!
//! * [`Reaction::Handled`] - the event was consumed; optionally emit effects;
//!   stay in the current state (an *internal transition*).
//! * [`Reaction::Ignored`] - the event was not applicable here.
//! * [`Reaction::Transition`] - take a transition to `target`, emitting the
//!   transition-action `effects` after exit actions and before entry actions.
//! * [`Reaction::Super`] - this handler does not deal with the event; the engine
//!   should re-dispatch it to `parent` (hierarchical event propagation).

use crate::effect::Effect;

/// What a state handler did with an event. Generic over the machine's opaque
/// `StateId`.
#[derive(Debug, Clone, PartialEq)]
pub enum Reaction<S> {
    /// Consumed the event; stay put; emit zero or more effects.
    Handled(Vec<Effect>),
    /// Event not applicable in this state.
    Ignored,
    /// Take a transition to `target`.
    Transition {
        /// Destination state.
        target: S,
        /// Effects fired by the transition action (after exits, before entries).
        effects: Vec<Effect>,
        /// Human-readable reason, recorded in the audit trail.
        rationale: String,
    },
    /// Defer to the superstate `parent`.
    Super(S),
}

impl<S> Reaction<S> {
    /// Convenience: consumed with no effects.
    #[must_use]
    pub fn handled() -> Self {
        Reaction::Handled(Vec::new())
    }

    /// Convenience: consumed, emitting `effects`.
    #[must_use]
    pub fn effects(effects: impl IntoIterator<Item = Effect>) -> Self {
        Reaction::Handled(effects.into_iter().collect())
    }

    /// Convenience: a transition with no effects and a default rationale.
    #[must_use]
    pub fn goto(target: S) -> Self {
        Reaction::Transition {
            target,
            effects: Vec::new(),
            rationale: String::new(),
        }
    }

    /// Convenience: a transition emitting `effects`, with `rationale`.
    #[must_use]
    pub fn transition(
        target: S,
        effects: impl IntoIterator<Item = Effect>,
        rationale: impl Into<String>,
    ) -> Self {
        Reaction::Transition {
            target,
            effects: effects.into_iter().collect(),
            rationale: rationale.into(),
        }
    }

    /// Convenience: defer to `parent`.
    #[must_use]
    pub fn parent(parent: S) -> Self {
        Reaction::Super(parent)
    }

    /// `true` if this reaction is a transition.
    #[must_use]
    pub fn is_transition(&self) -> bool {
        matches!(self, Reaction::Transition { .. })
    }
}
