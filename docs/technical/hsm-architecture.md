# Hierarchical State Machine Architecture

Sven's agent loop is a formally-specified **Hierarchical State Machine (HSM)**,
not a free-running LLM loop. This document explains the design, why it was
chosen, and how every major component fits into it.

---

## The core insight

Calling an LLM in a loop resembles the old embedded-systems *superloop*
architecture: a single `while true` that polls everything and hopes nothing
blocks. That style scales poorly because control flow is implicit, testing
requires mocking I/O, and any new capability has to be wired directly into the
loop body.

The HSM architecture inverts this:

| Concern | Owner |
|---------|-------|
| **What state the session is in** | HSM kernel (deterministic, pure) |
| **What to do next** | LLM reasoning service (untrusted, typed) |
| **How to perform I/O** | Effect executors (the only place I/O happens) |

The LLM never decides to call a tool. It produces a typed *proposal* (e.g.
`IntentExtraction` or `PatchProposal`), the HSM decides what to do with it
based on the current state and guards, and the resulting *Effects* are
dispatched to executors. This gives deterministic, auditable, testable control
flow with LLM intelligence at the right inflection points.

---

## Key concepts

### Events

Events are the sole input to the HSM. Every user keystroke, LLM response, tool
result, timer expiry, and network message becomes a typed `Event` before it
touches the machine. The `Event` enum is defined in `sven-hsm` and carries a
`SessionId`, `EventId` (UUID), `timestamp`, and an `EventKind` payload.

```
UserMessage(text)          HumanApproved / HumanRejected
LlmResponse(typed payload) UserCancelled
ToolSucceeded / ToolFailed TimeoutFired
TeamEvent(gossip)          Internal(Custom)
```

### Effects

Transitions emit `Vec<Effect>`. Effects are the only mechanism for I/O. An
effect is data, not a function call - executors perform the actual work after
the machine has advanced its state. This keeps transition functions *pure*:
given `(state, event)` → `(new_state, effects)`.

Common effects:

```
CallLlm { request: LlmRequest }          AskUser { question }
CallTool { name, args }                  RequestHumanApproval { description }
ScheduleTimeout / CancelTimeout          CreateCheckpoint / RollbackToCheckpoint
PersistAudit { record }                  EmitInternal { event }
InstantiateSubmachine { machine_id }     SubmachineCompleted { result }
```

### Permission policy

Before any effect is executed, `validate_effects_are_allowed` checks every
`Effect` against the active `PermissionPolicy` for the current machine. A
`ToolCapability` set is attached per state family; states that should never call
shell commands simply do not have that capability. This makes forbidden tool
calls architecturally impossible, not just conventionally avoided.

### Audit and replay

Every `(session_id, event_id, state_before, state_after, effects)` tuple is
written as an `AuditRecord` to an append-only JSONL log. The `replay` function
in `sven-hsm` can reconstruct any machine state deterministically from the
log, enabling post-mortem debugging and regression tests without mocking.

---

## Runtime: the Active Object

The HSM runs inside a tokio Active Object - a single consumer task that owns
the machine and processes events sequentially. This guarantees **Run-to-
Completion (RTC)** semantics: one event is fully processed (transition executed,
effects collected) before the next is dispatched.

```
  ┌─────────────────────────────────────────────────────────────┐
  │  tokio::sync::mpsc  ←─ EventSink (cloneable, multi-producer) │
  │                                                              │
  │  Consumer task                                               │
  │    1. pop Event from queue                                   │
  │    2. dispatch_event(machine, event) → effects               │
  │    3. validate_effects_are_allowed(policy, effects)?         │
  │    4. executor.execute(effects, event_sink.clone())          │
  │    5. emit AuditRecord                                       │
  └─────────────────────────────────────────────────────────────┘
```

Executors run their work on separate tokio tasks and post result events back
to the same queue via `EventSink`. This means all async I/O is outside the
machine; the machine itself is always synchronous.

---

## Dispatch algorithm

Sven uses Samek's two-phase HSM algorithm from *Practical UML Statecharts in
C/C++* (implemented in pure Rust in `sven-hsm/src/dispatch.rs`):

1. **Super-chain walk**: find the innermost ancestor that handles the event.
2. **LCA computation**: find the Least Common Ancestor of the source and
   target states.
3. **Exit sequence**: call exit actions for every state from the source up to
   (but not including) the LCA.
4. **Entry sequence**: call entry actions for every state from the LCA down to
   the target.
5. **Init drilling**: if the target is a composite, keep calling `initial()`
   until a leaf state is reached.

Entry and exit handlers may emit effects but may never cause a transition
(enforced by `debug_assert`). All effects are collected into a single
`Vec<Effect>` returned by `dispatch_event`.

---

## Machines

Two machines ship out of the box, both implemented in `sven-core/src/machines/`.

### ConversationMachine (mode: `chat`)

The default interactive chat machine. Five states:

```
Top
└── Active
    ├── Idle              ← awaiting user input
    ├── Interpreting      ← LLM extracting intent from message
    ├── Responding        ← LLM generating a response / tool calls
    ├── AwaitingTool      ← waiting for one or more tool results
    └── AwaitingUser      ← agent asked a clarifying question
```

When the LLM identifies a large engineering task during `Interpreting`, the
machine emits `InstantiateSubmachine(SoftwareDevelopmentMachine)` and suspends
until the submachine completes.

### SoftwareDevelopmentMachine (mode: `sdlc`)

The full software-development lifecycle machine. 57 states encoding a
formal engineering workflow:

```
Top
├── Intake
│   ├── InterpretUserIntent
│   ├── ExtractProblemStatement
│   ├── ExtractConstraints
│   ├── AssessInformationCompleteness
│   └── ConfirmScope
├── Discovery
├── Planning
├── Execution
│   ├── ProposePatch
│   ├── ApplyPatch
│   ├── Build
│   ├── RunTests
│   ├── StaticAnalysis
│   ├── ObserveResult
│   └── DecideTaskOutcome
├── Verification
├── Delivery
├── Recovery
│   ├── ClassifyFailure
│   ├── ProposeRecoveryOptions
│   └── SelectRecoveryAction
├── RollingBack
├── AwaitUser             ← Continuation-based: resumes prior state after answer
├── AwaitHumanApproval    ← gate before any destructive change
├── AwaitTool
├── Done
├── Failed
└── Cancelled
```

**Execution loop detail:**

```
ProposePatch → ApplyPatch → Build
                              ├─ success → RunTests
                              │              ├─ pass → StaticAnalysis → ObserveResult
                              │              └─ fail → Recovery
                              └─ fail → Recovery
```

**Continuation-based `AwaitUser`:** when any state needs more information, it
stores a `Continuation { return_to_state, context_key }` in extended state and
transitions to `AwaitUser`. When the user answers, the machine restores the
continuation and re-enters the correct state, never losing track of where it
was.

**`AwaitHumanApproval`:** entered before applying any patch with externally
visible effects. The `RequestHumanApproval` effect reaches the `UserExecutor`,
which surfaces a prompt in the TUI (`UiMode::AwaitingApproval`). Approval
resumes execution; rejection triggers `Recovery`.

### ClarificationMachine (submachine)

A reusable five-state submachine used by any state needing more information:

```
GeneratingQuestion → AwaitingAnswer → InterpretingAnswer → Deciding → Done
```

Any state can emit `InstantiateSubmachine(ClarificationMachine)`. When it
reaches `Done`, it posts `SubmachineCompleted` back to the parent, which
resumes from wherever it was waiting.

---

## ModeRegistry

`ModeRegistry` maps mode strings to `Machine` factories. Adding a new machine
is a single `registry.register("my-mode", || Box::new(MyMachine::new()))` call.
Pre-registered modes:

| Mode string | Machine |
|-------------|---------|
| `"chat"` | `ConversationMachine` |
| `"sdlc"` | `SoftwareDevelopmentMachine` |

The mode is selected at startup from (in priority order):
1. `--mode <name>` CLI flag
2. `SVEN_MODE` environment variable
3. Default: `"chat"`

---

## LLM as an untrusted reasoning service (`sven-llm`)

The `sven-llm` crate defines 11 named `LlmRequest` operations. Each serialises
to a structured prompt plus a JSON output schema. The LLM returns a structured
JSON value, which is parsed into a typed response and wrapped in the appropriate
`Event` variant. The LLM never names a tool; it only fills in fields of known
response structs.

| Request | Response type | Purpose |
|---------|--------------|---------|
| `ExtractIntent` | `IntentExtraction` | Classify user message, detect if SDLC task |
| `AssessCompleteness` | `CompletenessAssessment` | Decide if enough info to start |
| `GenerateClarifyingQuestion` | `ClarifyingQuestion` | Ask targeted follow-ups |
| `ProposePatch` | `PatchProposal` | Produce a unified diff |
| `AssessBuildResult` | `BuildAssessment` | Interpret compiler output |
| `AssessTestResult` | `TestAssessment` | Classify pass/fail/flaky |
| `ProposeRecovery` | `RecoveryProposal` | Suggest retry/rollback/abort |
| `GenerateDeliveryNote` | `DeliveryNote` | Write a change summary |
| `ClassifyFailure` | `FailureClassification` | Root-cause a failure |
| `SummarizeDiscovery` | `DiscoverySummary` | Summarise findings for planning |
| `ValidatePlan` | `PlanValidation` | Critique a proposed plan |

`MockLlmAdapter` is a scripted queue of pre-programmed events. All machine
unit tests use it - no real LLM or network call is needed.

---

## Effect executors (`sven-executors`)

Each executor implements `EffectExecutor` and handles a subset of `EffectKind`
values. The `CompositeExecutor` (built by `RuntimeBuilder`) routes each effect
to the right sub-executor:

| Executor | Effects handled |
|----------|----------------|
| `LlmExecutor` | `CallLlm` |
| `ToolExecutor` | `CallTool` (capability-checked) |
| `UserExecutor` | `AskUser`, `RequestHumanApproval` |
| `TimerExecutor` | `ScheduleTimeout`, `CancelTimeout` |
| `CheckpointExecutor` | `CreateCheckpoint`, `RollbackToCheckpoint` |
| `AuditExecutor` | `PersistAudit` |
| `InternalExecutor` | `EmitInternal` |

---

## Outward observation plane (`ObservationBus` / `UiEvent`)

The kernel emits two streams outward:

| Stream | Transport | Purpose |
|--------|-----------|---------|
| `MachineProjection` | `watch` channel | Coarse state snapshot for rendering the UI shell (mode, overlay, status) |
| `UiEvent` | `broadcast` channel (`ObservationSink`) | Fine-grained streaming events (text deltas, tool progress, token usage) |

`UiEvent` variants:

```
TextDelta(String)              // streamed text chunk
TextComplete(String)           // full accumulated response text
ThinkingDelta / ThinkingComplete  // extended thinking support
ToolStarted { call_id, name, args }
ToolProgress { call_id, message }
ToolFinished { call_id, name, output, is_error }
TokenUsage { input, output, cache_read, cache_write, ... }
ContextCompacted { ... }       // after automatic context compaction
TodoUpdate(Value)              // updated todo list (JSON array)
ModeChanged(String)            // e.g. "plan", "agent", "research"
ModelChanged(String)           // e.g. "claude-opus-4-5"
Error(String)                  // recoverable error
TurnComplete                   // the current turn is finished
```

`ObservationSink` wraps a `broadcast::Sender<UiEvent>`. Frontends subscribe
with `handle.subscribe_observations()` and receive events until `TurnComplete`
or `Error`. Lagged subscribers skip dropped events (the bus is lossy by
design, just like a render-tick stream).

---

## `ReactiveAgentMachine` + `ConverseExecutor` (Path B architecture)

Rather than reimplementing the full agentic loop as HSM states, the
`ReactiveAgentMachine` (in `sven-core`) uses **Path B**: it wraps the
existing `sven_core::Agent` via `ConverseExecutor` and delegates the
LLM⇆Tool loop to it.

```
┌────────────────── HSM kernel ─────────────────────┐
│  ReactiveAgentMachine                              │
│    Idle ──UserMessage──► Generating                │
│                          │ Effect::CallLlm { ... } │
└──────────────────────────┼────────────────────────┘
                           │
           ┌───────────────▼─────────────────────────┐
           │ ConverseExecutor                         │
           │  calls Agent::submit() in a loop         │
           │  translates AgentEvent → UiEvent         │
           │  posts Event::TurnComplete when done     │
           └─────────────────────────────────────────┘
```

This means the legacy `Agent` / `run_agentic_loop` is still alive **inside**
`ConverseExecutor`, not deleted. It will be replaced by a native HSM
implementation in a future phase when all frontends are confirmed stable on
the kernel path.

---

## Multi-session supervisor (`SessionSupervisor`)

`SessionSupervisor` in `sven-bootstrap` manages a registry of concurrent
kernel sessions keyed by `SessionId`:

```
SessionId → SessionBundle {
    runtime:        ErasedRuntime,      // kernel task (detached tokio task)
    handle:         RuntimeHandle,      // cheap clone for posting events
    channels:       KernelChannels,     // question_rx / approval_rx
    converse_agent: Option<Arc<Mutex<Agent>>>  // exposed for history ops
}
```

Each session has its own kernel, model provider, MCP manager, tool registry,
and observation bus. Sessions share `SharedSkills`, `SharedKnowledge`, and
`SharedAgents` (all reference-counted) to avoid redundant disk reads.

`RuntimeBuilder` is the per-session factory:

```rust
let bundle = RuntimeBuilder::new(config, "agent")
    .with_runtime_context(ctx)
    .with_tool_question_tx(question_tx)     // TUI question modal
    .with_permission_requester(perm)        // ACP IDE approval
    .with_initial_history(messages)         // resume a session
    .build_session()
    .await?;
```

---

## UI integration

The TUI is **not** part of the machine. It is a projection consumer and an
event source:

- **Event source**: keystrokes, approval decisions, and user text are posted to
  the kernel queue as typed `Event` values via `EventSink`.
- **Projection consumer**: the runtime broadcasts `MachineProjection` snapshots
  (a UI-friendly view of kernel state) after every dispatch. The TUI renders
  from snapshots, never by inspecting internal machine state.

`UiMode` in `sven-tui` is a single enum that covers every overlay the TUI can
show. It is set directly from the incoming `MachineProjection`:

```rust
enum UiMode {
    Normal,
    AwaitingUserInput { question: String },
    AwaitingApproval  { request: ApprovalRequest },
    Pager,
    Inspector,
    SearchActive,
    EditSegment,
    EditQueue,
    TeamPicker,
    Confirm { action: ConfirmAction },
}
```

---

## CI / headless mode

`RuntimeRunner` in `sven-ci` drives the kernel without any UI. It:
1. Posts the initial prompt as `Event::UserMessage`.
2. Auto-responds to every `RequestHumanApproval` effect (configurable via
   `RuntimeRunnerOptions::auto_approve`).
3. Collects `MachineProjection` updates until the machine reaches `Done`,
   `Failed`, or `Cancelled`.
4. Returns exit code 0 for `Done`, non-zero otherwise.

---

## Testing

Because transition functions are pure, unit tests need only construct an event
and assert on the resulting `(new_state, effects)` tuple:

```rust
let mut machine = ConversationMachine::new();
let effects = dispatch_event(&mut machine, Event::user_message("Fix the bug"));
assert_eq!(machine.state(), State::Interpreting);
assert!(effects.iter().any(|e| matches!(e, Effect::CallLlm { .. })));
```

Integration tests replay an `AuditRecord` log and assert that the final state
matches expectations. E2E bats tests use `--model mock` with a
`MockLlmAdapter` scripted response queue - no real LLM or API key required.

---

## Crates

| Crate | Role |
|-------|------|
| `sven-hsm` | HSM kernel: dispatch, Machine trait, Runtime (Active Object), permissions, audit, replay, `ObservationSink`/`UiEvent` |
| `sven-llm` | Typed LLM request/response contracts + `LlmAdapter` trait + `MockLlmAdapter` |
| `sven-executors` | Effect executors: LLM (`ConverseExecutor`), tool, user, timer, checkpoint, audit, internal, composite |
| `sven-core` | Concrete machines: `ReactiveAgentMachine`, `ConversationMachine`, `SoftwareDevelopmentMachine`, `ClarificationMachine`, `ModeRegistry` |
| `sven-bootstrap` | `RuntimeBuilder` (per-session factory), `SessionSupervisor` (multi-session registry) |
| `sven-frontend` | `kernel_session_task` - bridges kernel `UiEvent`s to `AgentEvent`s for TUI/GUI renderers |
| `sven-ci` | `RuntimeRunner` - headless kernel driver for batch/CI runs |
| `sven-node` | `ControlService` - routes operator commands to the kernel; `ui_event_to_control` bridge |
| `sven-acp` | `SvenAcpAgent` - ACP server backed by a per-session kernel; `ui_event_to_session_update` bridge |

---

## Further reading

- Miro Samek, *Practical UML Statecharts in C/C++, 2nd ed.* (the dispatch
  algorithm implemented in `sven-hsm/src/dispatch.rs` follows Chapter 2-4)
- [sven-hsm tests](../../crates/sven-hsm/tests/) - 31 pure unit/integration
  tests covering LCA ordering, permission rejection, replay equality, and
  virtual-time timeout behaviour
- [ConversationMachine source](../../crates/sven-core/src/machines/conversation.rs)
- [SoftwareDevelopmentMachine source](../../crates/sven-core/src/machines/software_development.rs)
