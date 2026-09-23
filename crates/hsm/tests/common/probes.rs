// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Small machines that probe the engine's edges rather than demonstrate it.
//!
//! [`AgentMachine`](super::AgentMachine) is a well-formed hierarchy used to
//! show the algorithm doing the right thing. The machines here exist to pin
//! down what the engine does with inputs the `Machine` contract does not, or
//! barely, allow: initial transitions that form a cycle, and a child that is
//! already finished the moment it is built.

use sven_hsm::event::InternalEvent;
use sven_hsm::{Context, Event, Machine, MachineId, Reaction};

// ---------------------------------------------------------------------------
// CyclicInitMachine - a machine that violates the initial-transition contract.
//
// `Machine::initial` is documented as "a proper descendant of top", and the
// same is implied of every composite's `Init` target - but nothing in the type
// system says so, and the engine follows whatever `Init` returns. Two states
// whose `Init` name each other is the smallest machine that turns that into a
// loop the engine used to take forever.
// ---------------------------------------------------------------------------

/// States of [`CyclicInitMachine`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CyclicState {
    /// Implicit root.
    Top,
    /// Whose `Init` names [`CyclicState::B`].
    A,
    /// Whose `Init` names [`CyclicState::A`].
    B,
}

/// A machine whose initial transitions form a cycle.
#[derive(Debug)]
pub struct CyclicInitMachine {
    id: MachineId,
}

impl CyclicInitMachine {
    /// Creates a new instance with a fresh [`MachineId`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }
}

impl Default for CyclicInitMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine for CyclicInitMachine {
    type State = CyclicState;

    fn id(&self) -> MachineId {
        self.id
    }
    fn top(&self) -> Self::State {
        CyclicState::Top
    }
    fn initial(&self) -> Self::State {
        CyclicState::A
    }
    fn superstate(&self, _state: Self::State) -> Self::State {
        CyclicState::Top
    }
    fn all_states(&self) -> Vec<Self::State> {
        vec![CyclicState::A, CyclicState::B]
    }
    fn dispatch_state(
        &mut self,
        state: Self::State,
        event: &Event,
        _ctx: &mut Context,
    ) -> Reaction<Self::State> {
        match (state, event) {
            (CyclicState::A, Event::Internal(InternalEvent::Init)) => {
                Reaction::goto(CyclicState::B)
            }
            (CyclicState::B, Event::Internal(InternalEvent::Init)) => {
                Reaction::goto(CyclicState::A)
            }
            _ => Reaction::Ignored,
        }
    }
}

// ---------------------------------------------------------------------------
// DoneOnArrivalMachine - a child whose work was already finished.
//
// A factory that builds a child for a task which turns out to be complete -
// a retried step that already succeeded, a phase whose postcondition already
// holds - returns a machine whose initial transition lands straight in its
// terminal state. The host has to notice that without being handed an event.
// ---------------------------------------------------------------------------

/// States of [`DoneOnArrivalMachine`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DoneOnArrivalState {
    /// Implicit root.
    Top,
    /// Terminal, and entered by the initial transition.
    Done,
}

/// A child machine that is terminal as soon as it is initialized.
#[derive(Debug)]
pub struct DoneOnArrivalMachine {
    id: MachineId,
}

impl DoneOnArrivalMachine {
    /// Creates a new instance with a fresh [`MachineId`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }
}

impl Default for DoneOnArrivalMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine for DoneOnArrivalMachine {
    type State = DoneOnArrivalState;

    fn id(&self) -> MachineId {
        self.id
    }
    fn top(&self) -> Self::State {
        DoneOnArrivalState::Top
    }
    fn initial(&self) -> Self::State {
        DoneOnArrivalState::Done
    }
    fn superstate(&self, _state: Self::State) -> Self::State {
        DoneOnArrivalState::Top
    }
    fn is_terminal(&self, state: Self::State) -> bool {
        state == DoneOnArrivalState::Done
    }
    fn all_states(&self) -> Vec<Self::State> {
        vec![DoneOnArrivalState::Done]
    }
    fn dispatch_state(
        &mut self,
        _state: Self::State,
        _event: &Event,
        _ctx: &mut Context,
    ) -> Reaction<Self::State> {
        Reaction::Ignored
    }
}
