//! Hierarchical composition: running a machine inside a state of another.
//!
//! A parent state may *own* a child submachine. While a child is active, events
//! are routed to the **child first**; anything the child does not handle bubbles
//! up to the parent (the orthogonal/submachine pattern). When the child reaches
//! its terminal state, the parent is notified with
//! [`InternalEvent::SubmachineCompleted`](crate::event::InternalEvent::SubmachineCompleted)
//! and the child is dropped.
//!
//! Because the kernel's [`Machine`] has an associated `State` type, child
//! machines are stored behind the object-safe [`ErasedMachine`] trait, which
//! erases the concrete state type while still exposing everything the host
//! needs.

use crate::context::Context;
use crate::dispatch::{DispatchOutcome, Hsm};
use crate::effect::Effect;
use crate::event::{Event, InternalEvent};
use crate::ids::MachineId;
use crate::machine::Machine;

/// Type-erased view of a running machine. Implemented for every
/// [`Hsm<M>`](crate::dispatch::Hsm); lets a host drive a child without knowing
/// its concrete `State` type.
pub trait ErasedMachine: Send {
    /// The child machine's identity.
    fn id(&self) -> MachineId;
    /// Fire initial transitions; returns entry effects.
    fn init(&mut self, ctx: &mut Context) -> Vec<Effect>;
    /// Dispatch an ordinary event.
    fn dispatch(&mut self, event: &Event, ctx: &mut Context) -> DispatchOutcome;
    /// Current state label.
    fn state_label(&self) -> String;
    /// `true` if the machine reached a terminal state.
    fn is_done(&self) -> bool;
}

impl<M> ErasedMachine for Hsm<M>
where
    M: Machine + Send,
    M::State: Send,
{
    fn id(&self) -> MachineId {
        self.machine().id()
    }

    fn init(&mut self, ctx: &mut Context) -> Vec<Effect> {
        Hsm::init(self, ctx)
    }

    fn dispatch(&mut self, event: &Event, ctx: &mut Context) -> DispatchOutcome {
        Hsm::dispatch(self, event, ctx)
    }

    fn state_label(&self) -> String {
        Hsm::state_label(self)
    }

    fn is_done(&self) -> bool {
        Hsm::is_done(self)
    }
}

/// The result of routing one event through a [`Submachine`].
#[derive(Debug, Clone)]
pub struct SubmachineOutcome {
    /// All effects produced (child first, then bubbled-parent, then any
    /// completion handling), in order.
    pub effects: Vec<Effect>,
    /// Parent state label after routing.
    pub parent_state: String,
    /// `true` if a child was active when the event arrived.
    pub child_was_active: bool,
    /// `true` if the child completed during this dispatch (and was dropped).
    pub child_completed: bool,
}

/// A parent machine plus an optional active child submachine.
pub struct Submachine<P: Machine> {
    parent: Hsm<P>,
    child: Option<Box<dyn ErasedMachine>>,
}

impl<P: Machine> Submachine<P> {
    /// Wraps a parent machine. Call [`init`](Submachine::init) before
    /// dispatching events.
    pub fn new(parent: P) -> Self {
        Self {
            parent: Hsm::new(parent),
            child: None,
        }
    }

    /// Initializes the parent machine.
    pub fn init(&mut self, ctx: &mut Context) -> Vec<Effect> {
        self.parent.init(ctx)
    }

    /// Installs and initializes a child submachine. Returns the child's entry
    /// effects. Replaces any existing child.
    pub fn instantiate_child(
        &mut self,
        mut child: Box<dyn ErasedMachine>,
        ctx: &mut Context,
    ) -> Vec<Effect> {
        let effects = child.init(ctx);
        self.child = Some(child);
        effects
    }

    /// `true` if a child submachine is currently active.
    pub fn has_child(&self) -> bool {
        self.child.is_some()
    }

    /// Shared access to the parent HSM.
    pub fn parent(&self) -> &Hsm<P> {
        &self.parent
    }

    /// The parent's current state.
    pub fn parent_state(&self) -> P::State {
        self.parent.state()
    }

    /// The active child's state label, if any.
    pub fn child_state_label(&self) -> Option<String> {
        self.child.as_ref().map(|c| c.state_label())
    }

    /// Routes `event`: to the child first (bubbling unhandled events to the
    /// parent), or directly to the parent when no child is active. On child
    /// completion the parent receives a `SubmachineCompleted` internal event.
    pub fn dispatch(&mut self, event: &Event, ctx: &mut Context) -> SubmachineOutcome {
        let mut effects = Vec::new();
        let child_was_active = self.child.is_some();
        let mut child_completed = false;

        if let Some(child) = self.child.as_mut() {
            let child_out = child.dispatch(event, ctx);
            effects.extend(child_out.effects);

            // Bubble events the child ignored up to the parent.
            if !child_out.handled {
                let parent_out = self.parent.dispatch(event, ctx);
                effects.extend(parent_out.effects);
            }

            // On terminal child, drop it and notify the parent.
            if child.is_done() {
                let id = child.id();
                self.child = None;
                child_completed = true;
                let completed = Event::Internal(InternalEvent::SubmachineCompleted {
                    machine: id.as_uuid().to_string(),
                });
                let parent_out = self.parent.dispatch(&completed, ctx);
                effects.extend(parent_out.effects);
            }
        } else {
            let parent_out = self.parent.dispatch(event, ctx);
            effects.extend(parent_out.effects);
        }

        SubmachineOutcome {
            effects,
            parent_state: self.parent.state_label(),
            child_was_active,
            child_completed,
        }
    }
}
