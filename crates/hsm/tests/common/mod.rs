//! Shared example machines for the integration tests.
//!
//! [`AgentMachine`] is the worked 3-level (actually up-to-4-level) hierarchy the
//! engine tests exercise; [`TimerMachine`] is a minimal machine for the
//! virtual-time runtime test.

#![allow(dead_code)]

use serde_json::Value;

use sven_hsm::event::InternalEvent;
use sven_hsm::{
    Context, Effect, Event, EventKind, Machine, MachineId, Reaction, TimerId, ToolCallId,
    ToolCapability,
};

// ---------------------------------------------------------------------------
// AgentMachine - a deep hierarchy used to exercise the full HSM algorithm.
//
// Hierarchy:
//   Top
//    +- Session                       (UserCancelled -> Done, inherited by all)
//    |    +- Conversation  (composite, init -> Listening)
//    |    |     +- Listening
//    |    |     +- Drafting
//    |    +- Task          (composite, init -> Planning)
//    |          +- Planning
//    |          +- Executing (composite, init -> Running)
//    |                +- Running
//    |                +- Verifying
//    +- Done                          (terminal)
// ---------------------------------------------------------------------------

/// States of the example agent machine.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum St {
    /// Implicit root.
    Top,
    /// Whole-session superstate.
    Session,
    /// Conversation composite.
    Conversation,
    /// Waiting for user input.
    Listening,
    /// Drafting a reply.
    Drafting,
    /// Task-execution composite.
    Task,
    /// Planning the task.
    Planning,
    /// Execution composite.
    Executing,
    /// Running a tool.
    Running,
    /// Verifying a result.
    Verifying,
    /// Terminal state.
    Done,
}

/// A fixed tool-call id so emitted `CallTool` effects compare deterministically.
#[must_use]
pub fn fixed_tool_id() -> ToolCallId {
    ToolCallId::from_uuid(uuid_from(0xA0))
}

fn uuid_from(byte: u8) -> uuid::Uuid {
    uuid::Uuid::from_bytes([byte; 16])
}

/// `enter:<State>` trace marker effect.
#[must_use]
pub fn enter(st: St) -> Effect {
    Effect::EmitInternal {
        name: format!("enter:{st:?}"),
        payload: Value::Null,
    }
}

/// `exit:<State>` trace marker effect.
#[must_use]
pub fn exit(st: St) -> Effect {
    Effect::EmitInternal {
        name: format!("exit:{st:?}"),
        payload: Value::Null,
    }
}

/// The `CallLlm` effect used as a transition action.
#[must_use]
pub fn call_llm() -> Effect {
    Effect::CallLlm {
        request: Value::Null,
    }
}

/// A `CallTool` effect exercising the `ExecuteShell` capability.
#[must_use]
pub fn call_shell() -> Effect {
    Effect::CallTool {
        call_id: fixed_tool_id(),
        name: "shell".into(),
        capability: ToolCapability::ExecuteShell,
        args: Value::Null,
    }
}

/// The example agent machine.
pub struct AgentMachine {
    id: MachineId,
}

impl Default for AgentMachine {
    fn default() -> Self {
        Self {
            id: MachineId::new(),
        }
    }
}

impl AgentMachine {
    /// Creates a fresh machine.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The transitions this machine declares, as `(source_leaf, event, target)`
    /// triples. Used by the transition-coverage test. For the inherited
    /// `UserCancelled` transition the `source_leaf` is the concrete leaf the
    /// test triggers it from.
    #[must_use]
    pub fn declared_transitions() -> Vec<(St, EventKind, St)> {
        use EventKind::*;
        use St::*;
        vec![
            (Listening, UserProvidedArtifact, Listening), // self-transition
            (Listening, UserMessage, Drafting),
            (Drafting, LlmProposedResponse, Listening),
            (Drafting, LlmProposedPlan, Planning),
            (Planning, LlmProposedToolCall, Running),
            (Running, ToolSucceeded, Verifying),
            (Running, ToolFailed, Planning),
            (Running, LlmProposedResponse, Listening), // deepest cross-superstate
            (Running, UserCancelled, Done),            // inherited via Session
            (Verifying, HumanApproved, Done),
            (Verifying, HumanRejected, Running),
        ]
    }
}

impl Machine for AgentMachine {
    type State = St;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> St {
        St::Top
    }

    fn initial(&self) -> St {
        // Root's initial transition targets the Session superstate; the engine
        // then drills Session -> Conversation -> Listening via Init.
        St::Session
    }

    fn superstate(&self, state: St) -> St {
        use St::*;
        match state {
            Top => Top,
            Session => Top,
            Conversation => Session,
            Listening => Conversation,
            Drafting => Conversation,
            Task => Session,
            Planning => Task,
            Executing => Task,
            Running => Executing,
            Verifying => Executing,
            Done => Top,
        }
    }

    fn is_terminal(&self, state: St) -> bool {
        state == St::Done
    }

    fn all_states(&self) -> Vec<St> {
        use St::*;
        // Every coverable state (Top is the implicit root, never an active leaf
        // nor explicitly entered).
        vec![
            Session,
            Conversation,
            Listening,
            Drafting,
            Task,
            Planning,
            Executing,
            Running,
            Verifying,
            Done,
        ]
    }

    fn dispatch_state(
        &mut self,
        state: St,
        event: &Event,
        _ctx: &mut sven_hsm::Context,
    ) -> Reaction<St> {
        use St::*;

        // Lifecycle entry/exit markers shared by every non-root state.
        macro_rules! lifecycle {
            ($st:expr) => {
                match event {
                    Event::Internal(InternalEvent::Entry) => return Reaction::effects([enter($st)]),
                    Event::Internal(InternalEvent::Exit) => return Reaction::effects([exit($st)]),
                    _ => {}
                }
            };
        }

        match state {
            Top => Reaction::Ignored,

            Session => {
                lifecycle!(Session);
                match event {
                    Event::Internal(InternalEvent::Init) => Reaction::goto(Conversation),
                    Event::UserCancelled => Reaction::transition(Done, [], "user cancelled"),
                    _ => Reaction::parent(Top),
                }
            }

            Conversation => {
                lifecycle!(Conversation);
                match event {
                    Event::Internal(InternalEvent::Init) => Reaction::goto(Listening),
                    _ => Reaction::parent(Session),
                }
            }

            Listening => {
                lifecycle!(Listening);
                match event {
                    Event::UserProvidedArtifact { .. } => {
                        Reaction::transition(Listening, [], "artifact noted")
                    }
                    Event::UserMessage { .. } => {
                        Reaction::transition(Drafting, [call_llm()], "draft a reply")
                    }
                    _ => Reaction::parent(Conversation),
                }
            }

            Drafting => {
                lifecycle!(Drafting);
                match event {
                    Event::LlmProposedResponse { .. } => Reaction::goto(Listening),
                    Event::LlmProposedPlan { .. } => {
                        Reaction::transition(Planning, [], "start task")
                    }
                    _ => Reaction::parent(Conversation),
                }
            }

            Task => {
                lifecycle!(Task);
                match event {
                    Event::Internal(InternalEvent::Init) => Reaction::goto(Planning),
                    _ => Reaction::parent(Session),
                }
            }

            Planning => {
                lifecycle!(Planning);
                match event {
                    Event::LlmProposedToolCall { .. } => {
                        Reaction::transition(Running, [call_shell()], "run the proposed tool")
                    }
                    _ => Reaction::parent(Task),
                }
            }

            Executing => {
                lifecycle!(Executing);
                match event {
                    Event::Internal(InternalEvent::Init) => Reaction::goto(Running),
                    _ => Reaction::parent(Task),
                }
            }

            Running => {
                lifecycle!(Running);
                match event {
                    Event::ToolSucceeded { .. } => Reaction::transition(Verifying, [], "verify"),
                    Event::ToolFailed { .. } => Reaction::transition(Planning, [], "replan"),
                    Event::LlmProposedResponse { .. } => {
                        Reaction::transition(Listening, [], "abandon task, reply")
                    }
                    _ => Reaction::parent(Executing),
                }
            }

            Verifying => {
                lifecycle!(Verifying);
                match event {
                    Event::HumanApproved { .. } => Reaction::transition(Done, [], "approved"),
                    Event::HumanRejected { .. } => Reaction::transition(Running, [], "redo"),
                    _ => Reaction::parent(Executing),
                }
            }

            Done => {
                lifecycle!(Done);
                // Terminal: ignore every domain event.
                Reaction::Ignored
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TimerMachine - minimal machine for the virtual-time test.
//
//   Top
//    +- Idle     (UserMessage -> Waiting)
//    +- Waiting  (entry schedules a 30s timeout; Timeout -> Fired)
//    +- Fired    (terminal)
// ---------------------------------------------------------------------------

/// States of the timer machine.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TState {
    /// Implicit root.
    Top,
    /// Idle, waiting to be kicked.
    Idle,
    /// Waiting for the scheduled timeout.
    Waiting,
    /// Terminal: timeout fired.
    Fired,
}

/// The fixed timer id the [`TimerMachine`] schedules.
#[must_use]
pub fn timer_id() -> TimerId {
    TimerId::from_uuid(uuid_from(0x77))
}

/// Minimal machine demonstrating a scheduled timeout.
pub struct TimerMachine {
    id: MachineId,
}

impl Default for TimerMachine {
    fn default() -> Self {
        Self {
            id: MachineId::new(),
        }
    }
}

impl TimerMachine {
    /// Creates a fresh timer machine.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Machine for TimerMachine {
    type State = TState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> TState {
        TState::Top
    }

    fn initial(&self) -> TState {
        TState::Idle
    }

    fn superstate(&self, _state: TState) -> TState {
        // A flat machine: every state's parent is the root.
        TState::Top
    }

    fn is_terminal(&self, state: TState) -> bool {
        state == TState::Fired
    }

    fn all_states(&self) -> Vec<TState> {
        vec![TState::Idle, TState::Waiting, TState::Fired]
    }

    fn dispatch_state(
        &mut self,
        state: TState,
        event: &Event,
        _ctx: &mut sven_hsm::Context,
    ) -> Reaction<TState> {
        use std::time::Duration;
        match state {
            TState::Top => Reaction::Ignored,
            TState::Idle => match event {
                Event::UserMessage { .. } => Reaction::transition(TState::Waiting, [], "wait"),
                _ => Reaction::parent(TState::Top),
            },
            TState::Waiting => match event {
                Event::Internal(InternalEvent::Entry) => {
                    Reaction::effects([Effect::ScheduleTimeout {
                        timer_id: timer_id(),
                        duration: Duration::from_secs(30),
                    }])
                }
                Event::Timeout { timer_id: t } if *t == timer_id() => {
                    Reaction::transition(TState::Fired, [], "timed out")
                }
                _ => Reaction::parent(TState::Top),
            },
            TState::Fired => Reaction::Ignored,
        }
    }
}

// ---------------------------------------------------------------------------
// GuardMachine - whose initial state's entry emits a dangerous tool call, used
// to test that the runtime's permission gate rejects effects before execution.
// ---------------------------------------------------------------------------

/// States of the guard machine.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GState {
    /// Implicit root.
    Top,
    /// Initial state; entry emits a forbidden `CallTool`.
    Acting,
}

/// Machine whose entry action requests a dangerous capability.
pub struct GuardMachine {
    id: MachineId,
}

impl Default for GuardMachine {
    fn default() -> Self {
        Self {
            id: MachineId::new(),
        }
    }
}

impl GuardMachine {
    /// Creates a fresh guard machine.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Machine for GuardMachine {
    type State = GState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> GState {
        GState::Top
    }

    fn initial(&self) -> GState {
        GState::Acting
    }

    fn superstate(&self, _state: GState) -> GState {
        GState::Top
    }

    fn dispatch_state(
        &mut self,
        state: GState,
        event: &Event,
        _ctx: &mut sven_hsm::Context,
    ) -> Reaction<GState> {
        match state {
            GState::Top => Reaction::Ignored,
            GState::Acting => match event {
                Event::Internal(InternalEvent::Entry) => Reaction::effects([call_shell()]),
                _ => Reaction::parent(GState::Top),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// ParentMachine - hosts a child submachine and reacts to its completion. Used
// by the submachine composition test.
// ---------------------------------------------------------------------------

/// States of the parent machine.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PState {
    /// Implicit root.
    Top,
    /// Working while a child submachine runs.
    Working,
    /// Terminal: child finished and the parent wrapped up.
    Finished,
}

/// Effect marker the parent emits when it handles a bubbled-up event.
#[must_use]
pub fn parent_handled() -> Effect {
    Effect::EmitInternal {
        name: "parent-handled".into(),
        payload: Value::Null,
    }
}

/// Parent machine for submachine composition tests.
pub struct ParentMachine {
    id: MachineId,
}

impl Default for ParentMachine {
    fn default() -> Self {
        Self {
            id: MachineId::new(),
        }
    }
}

impl ParentMachine {
    /// Creates a fresh parent machine.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Machine for ParentMachine {
    type State = PState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> PState {
        PState::Top
    }

    fn initial(&self) -> PState {
        PState::Working
    }

    fn superstate(&self, _state: PState) -> PState {
        PState::Top
    }

    fn is_terminal(&self, state: PState) -> bool {
        state == PState::Finished
    }

    fn dispatch_state(
        &mut self,
        state: PState,
        event: &Event,
        _ctx: &mut sven_hsm::Context,
    ) -> Reaction<PState> {
        match state {
            PState::Top => Reaction::Ignored,
            PState::Working => match event {
                Event::Internal(InternalEvent::SubmachineCompleted { .. }) => {
                    Reaction::transition(PState::Finished, [], "child completed")
                }
                // A bubbled-up event the child ignored but the parent handles.
                Event::HumanApproved { .. } => Reaction::effects([parent_handled()]),
                _ => Reaction::parent(PState::Top),
            },
            PState::Finished => Reaction::Ignored,
        }
    }
}

// ---------------------------------------------------------------------------
// OpaqueMachine - a machine that never opted into `all_states()`.
//
// This is what every machine written before state enumeration mattered looks
// like: correct, dispatchable, but with no way to map a state *name* back to a
// state value. Restoring one from a snapshot must fail loudly rather than
// silently resetting it to its initial state.
// ---------------------------------------------------------------------------

/// States of [`OpaqueMachine`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum OpaqueState {
    /// Implicit root.
    Top,
    /// The only real state.
    Idle,
}

/// A machine that leaves [`Machine::all_states`] at its empty default.
#[derive(Debug)]
pub struct OpaqueMachine {
    id: MachineId,
}

impl OpaqueMachine {
    /// Creates a new instance with a fresh [`MachineId`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }
}

impl Default for OpaqueMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine for OpaqueMachine {
    type State = OpaqueState;

    fn id(&self) -> MachineId {
        self.id
    }
    fn top(&self) -> Self::State {
        OpaqueState::Top
    }
    fn initial(&self) -> Self::State {
        OpaqueState::Idle
    }
    fn superstate(&self, _state: Self::State) -> Self::State {
        OpaqueState::Top
    }
    fn dispatch_state(
        &mut self,
        _state: Self::State,
        _event: &Event,
        _ctx: &mut Context,
    ) -> Reaction<Self::State> {
        Reaction::Handled(Vec::new())
    }
}
