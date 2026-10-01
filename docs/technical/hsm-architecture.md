# Hierarchical State Machine Architecture

Sven's agent runtime is built on a formally-specified **Hierarchical State
Machine (HSM)** kernel, not a free-running LLM loop. This document is the
reference for the kernel itself - its events, effects, permission model, audit
and replay, the Active Object runtime, the two-phase dispatch algorithm, the
observation plane, the crate map, and the `ModeRegistry` that decides which
machine drives each mode.

Two companion documents cover the concrete machines and subsystems:

- **[State Machine Reference](state-machines.md)** - complete states, events,
  effects, and transition tables for every machine in sven.
- **[Parallel Submachine Fan-out](parallel-submachines.md)** - how the kernel
  spawns isolated child kernels to run plan tasks concurrently.

---

## The core insight

Calling an LLM in a loop resembles the old embedded-systems *superloop*: a
single `while true` that polls everything and hopes nothing blocks. Control
flow is implicit, testing requires mocking I/O, and every new capability has to
be wired directly into the loop body.

The HSM architecture inverts this by separating three concerns:

| Concern | Owner |
|---------|-------|
| **What state the session is in** | HSM kernel (deterministic, pure) |
| **How to perform I/O** | Effect executors (the only place I/O happens) |
| **What to reason about next** | The LLM, invoked from inside an executor |

The kernel never performs I/O. A machine reacts to a typed `Event`, mutates its
extended state, and **returns** a `Vec<Effect>` describing the side effects it
wants. The runtime validates those effects against a permission policy and hands
each to an executor. Executors run the actual work on their own tasks and post
result `Event`s back into the queue. This gives deterministic, auditable,
testable control flow with intelligence injected at well-defined points.

> **Where does the LLM "decide to call a tool"?** In the **unified
> kernel-mediated model** (both `ReactiveAgentMachine` and `SdlcMachine`), the
> model *proposes* tool calls inside a single streaming pass run by `TurnExecutor`
> (`kind: "turn"`). Those proposals are returned as `ProposedToolCall`s in
> `Event::LlmTurnComplete`; the machine emits one `Effect::CallTool` per proposal;
> the kernel gates each call through the `PermissionPolicy`; and `ToolExecutor`
> executes allowed calls concurrently (spawn-and-forget). Each phase state handles
> the tool loop **in-state** (`Reaction::Handled`) - there are no separate
> `RunningTools` or `AwaitingApproval` states. No executor runs a multi-step tool
> loop or calls `registry.execute` on behalf of the kernel.

---

## Events - the only input

Events are the sole input to a machine. User messages, LLM/loop completions,
tool results, approvals, and timers all become a typed `Event`
(`hsm/src/event.rs`) before they touch the machine. Payloads that are
domain-specific are carried as opaque `serde_json::Value` so the kernel stays
domain-agnostic.

| Event | Meaning |
|-------|---------|
| `UserMessage { text }` | A human sent a chat/instruction message |
| `UserProvidedArtifact { artifact }` | A human attached an opaque artifact |
| `UserCancelled` | The human asked to cancel in-flight work |
| `LlmTurnComplete { thread, text, tool_calls }` | A `TurnExecutor` streaming pass finished; carries proposed tool calls |
| `LlmFailed { error }` | The model failed to produce a usable result |
| `ToolSucceeded { call_id, observation }` | A kernel-dispatched tool call succeeded |
| `ToolFailed { call_id, error }` | A kernel-dispatched tool call failed |
| `ToolApprovalRequired { call_id, capability, description }` | A tool call needs human approval before executing |
| `HumanApproved { approval_id }` | A human approved a pending request |
| `HumanRejected { approval_id }` | A human rejected a pending request |
| `QuestionAsked { call_id, prompt, options }` / `HumanAnswered { .. }` | A tool parked a question for the human, and its answer |
| `Timeout { timer_id }` | A scheduled timer elapsed |
| `VerificationComplete { verdict }` | An `Effect::Verify` finished evaluating |
| `EffectFailed { kind, error }` | An effect was refused or had no executor, and no more specific failure event exists for it (see below) |
| `Internal(InternalEvent)` | A kernel-internal lifecycle or composition signal |

`InternalEvent` carries the reserved framework signals plus composition signals:

| `InternalEvent` | Meaning |
|-----------------|---------|
| `Entry` / `Exit` / `Init` | Reserved HSM lifecycle signals (dispatched to a single handler by the engine, never propagated to a superstate) |
| `SubmachineCompleted { machine, result }` | A child submachine reached a terminal state; `result` carries the child's structured result payload for the parent to aggregate |
| `Custom { name, payload }` | A generic domain-internal signal |

Every event also has a payload-free `EventKind` discriminant used for audit
records and transition-coverage assertions.

---

## Effects - the only output

Transitions return `Vec<Effect>` (`hsm/src/effect.rs`). An effect is pure
data, not a function call; executors perform the work *after* the machine has
advanced. This keeps transition functions pure: `(state, event) → (new_state,
effects)`.

The complete effect vocabulary (11 variants):

| Effect | Purpose |
|--------|---------|
| `CallLlm { request }` | Ask the reasoning service for a result. `request` is opaque JSON; the `kind` field must be `"turn"` (handled by `TurnExecutor` - the only wired executor in production) |
| `CallTool { call_id, name, capability, args }` | Invoke a tool through the kernel. Requires the named `capability` to be permitted in the current state |
| `AskUser { prompt }` | Ask the human a question (non-blocking; the answer arrives as a later event) |
| `RequestHumanApproval { approval_id, capability, description }` | Request explicit approval before a dangerous capability is used |
| `RequestHumanAnswer { question_id, call_id, prompt, options }` | Park a tool's question for the human; the answer arrives as `HumanAnswered` |
| `ScheduleTimeout { timer_id, duration }` | Schedule a one-shot timer that posts `Timeout` |
| `CancelTimeout { timer_id }` | Cancel a scheduled timer |
| `PersistAudit` | Persist the audit log to durable storage |
| `EmitInternal { name, payload }` | Re-enter a domain-internal event into the queue |
| `InstantiateSubmachine { machine, descriptor }` | Spawn a child submachine and route its lifecycle through the runtime (see [Parallel Submachine Fan-out](parallel-submachines.md)) |
| `Verify { spec }` | Evaluate a declarative predicate against the real world; the verdict arrives as `VerificationComplete` |

Each effect exposes `kind()` (a payload-free `EffectKind` for audit/coverage)
and `required_capability()`: `CallTool` reports the capability of its tool,
and `InstantiateSubmachine` reports `SpawnChild`, so a state starts child runs
only where its policy allows it. Every other effect carries no capability.

---

## Permission policy - the single choke point

Before any effect in a batch is executed, `validate_effects_are_allowed`
(`hsm/src/permissions.rs`) checks it against the active `PermissionPolicy`.
The policy is keyed by the `Debug` label of the current state, so it works for
any machine's opaque state type without the kernel knowing the concrete type.

For `CallTool` effects the gate is **per-call** (not batch-wide). Each effect
independently receives one of three verdicts:

- `Allowed` → `ToolExecutor` executes the call (spawned concurrently).
- `Forbidden` → `Event::ToolFailed { call_id, error: "denied" }` is emitted
  immediately; the call never reaches the registry.
- `NeedsApproval` → the policy asks a person to approve this call and nobody
  has yet: `Event::ToolApprovalRequired` is emitted, the machine emits
  `Effect::RequestHumanApproval { approval_id, capability, description, call }`,
  on `HumanApproved` the call proceeds, on `HumanRejected` it is answered with
  the refusal.

Non-tool effects (`AskUser`, `PersistAudit`, timers, etc.) remain
all-or-nothing: a forbidden batch is recorded in the audit trail, not executed,
and each effect in it that a machine may be waiting on is answered with its
failure event (see [Effect executors](#effect-executors)).

Capabilities are coarse buckets (`ToolCapability`): `ReadFile`, `WriteFile`,
`DeleteFile`, `ExecuteShell`, `NetworkAccess`, `GitOperation`, `RunVerifier`,
`ControlDevice`, `SpawnChild`. What a state allows runs without anyone being
asked unless the policy requires approval for that capability
(`PermissionPolicy::with_manual_approval` requires it for every capability that
is not `ToolCapability::is_read_only` - everything but `ReadFile`,
`RunVerifier` and `SpawnChild`). An approval is for one call: `HumanApproved`
approves the call it was asked for (`PendingApproval::call_id`,
`Context::is_call_approved`), and the next call of the same capability is
asked about again. A `PermissionPolicy` is assembled with a builder
(`allow_in`, `allow_globally`, `require_approval`).


In production, `RuntimeBuilder` uses each machine's own `permission_policy()`
(`SdlcMachine::permission_policy()` / `ReactiveAgentMachine::permission_policy()`)
rather than a blanket open policy, giving each mode the minimum-required
capability set. This means the kernel permission gate is now **fully exercised**
for every tool call in every mode - there is no longer a separate
`ToolRegistry::ApprovalPolicy` / `PermissionRequester` path for kernel sessions.

---

## Audit and replay - the event-sourcing spine

Every dispatch appends exactly one `AuditRecord` (`hsm/src/audit.rs`) to
the `Context`. A record captures `from_state`, `to_state`, the `EventKind`, the
payload-free `EffectKind`s emitted, an optional rationale, and an `AuditOutcome`
(`Transition`, `InternalHandled`, `Ignored`, or `Rejected`). Rejected batches
are recorded for forensics but never executed.

Because the dispatch engine is pure (it returns effects rather than performing
them), `replay(factory, events)` deterministically reconstructs any machine's
state from the recorded input events. Lifecycle signals in the log are skipped
(the engine regenerates them). This enables post-mortem debugging and
regression tests that assert on whole session traces without a real LLM or
network.

The runtime mirrors the in-`Context` audit trail into a shared snapshot
(`audit_snapshot()`) and, when an `AuditExecutor` is wired, persists records to
an append-only JSONL log on the `PersistAudit` effect.

---

## Runtime - the Active Object

The kernel runs inside a tokio **Active Object** (`sven-kernel`, `kernel/src/lib.rs`): a
single consumer task that owns the machine and drains an `mpsc` event queue.
That single task is what guarantees **Run-to-Completion (RTC)**: one event is
fully processed (dispatched, effects validated, effects executed) before the
next is pulled.

```
  ┌──────────────────────────────────────────────────────────────┐
  │  EventSink (cloneable, multi-producer)  ──►  mpsc queue        │
  │                                                                │
  │  Consumer task (single):                                       │
  │    1. recv Event                                               │
  │    2. note child completion (submachine registry bookkeeping)  │
  │    3. machine.dispatch(event) → DispatchOutcome { effects }    │
  │    4. emit UiEvent::Transition on the observation plane        │
  │    5. run_effects(): classify each CallTool against policy;    │
  │       validate the other effects all-or-nothing                │
  │    6. InstantiateSubmachine → child spawner, with the contract │
  │       and a linked cancel scope; every other allowed effect    │
  │       → executor.execute(effect, sink, obs)                    │
  │    7. publish RuntimeStatus + audit snapshot                   │
  └──────────────────────────────────────────────────────────────┘
```

Executors run their work on separate tasks and post result events back through
a cloned `EventSink`. All async I/O is therefore outside the machine; the
machine itself is always synchronous.

There are two runtime flavours:

- `Runtime<M>` - generic over a concrete `Machine` type. Used in tests and
  wherever the machine type is known at compile time.
- `ErasedRuntime` - drives a `Box<dyn ErasedMachine>` whose state type is erased.
  Used in production where the machine is selected at runtime from the
  `ModeRegistry`.

Both expose `spawn` and `spawn_with_children` (the latter installs an optional
`ChildSpawner`), plus `post`, `status`/`status_watch`, `subscribe_observations`,
`wait_for_state`, `wait_done`, `audit_snapshot`, `abort`, `cancel`/`cancel_scope`
and `join`. `ErasedRuntime::spawn_child_run` starts a machine as the child run
of another (see [Child runs](#child-runs)).

Every runtime owns a `CancelScope`. Cancelling it stops the consumer loop
between events or in the middle of a dispatch's effects - the effects in
flight are dropped, including a model turn being streamed - and publishes a
`done` status whose `last_error` is `sven_kernel::RUN_CANCELLED`. The machine's
state label is left where the run stopped.

Timers are deterministic in tests via the `Clock` abstraction: a `SystemClock`
for production and a `VirtualClock` whose time only advances when a test calls
`advance`. `TimerService` computes the absolute deadline synchronously at
schedule time so advancing virtual time before the sleeping task starts is never
missed.

---

## Dispatch algorithm - two phases and a real LCA

Dispatch (`hsm/src/dispatch.rs`) is a faithful implementation of Samek's
two-phase HSM algorithm (from *Practical UML Statecharts in C/C++*), in pure
Rust:

1. **Phase 1 - find the handler.** Starting at the active leaf, call
   `dispatch_state`. While it returns `Reaction::Super(parent)`, re-dispatch to
   that parent. This walks the super-chain until some state handles the event
   (with a transition or an internal `Handled`) or the root ignores it.
2. **Phase 2 - execute the transition.** Compute the genuine **Least Common
   Ancestor** of the transition source and target by walking both ancestor
   chains (no depth-counting shortcut). Then, in order:
   - **Exit** actions bottom-up from the active leaf to the LCA (exclusive),
   - the **transition action** effects,
   - **Entry** actions top-down from below the LCA to the target,
   - **Init drilling**: repeatedly fire the target's `Init` transition until a
     leaf is reached.

Self-transitions are handled by treating the LCA as `superstate(source)`, which
forces exactly one exit/re-enter cycle.

A handler returns a `Reaction`:

- `Handled(effects)` - consumed the event, stay put, emit effects (internal
  transition).
- `Ignored` - not applicable here.
- `Transition { target, effects, rationale }` - take a transition.
- `Super(parent)` - defer to the superstate.

Entry/exit handlers may emit effects but must **never** transition (enforced by
a `debug_assert!`); only the `Init` signal may return a transition. Effects are
collected in execution order into a single `Vec<Effect>` returned by
`dispatch`, alongside the `AuditRecord` and `from`/`to` labels.

---

## The two data planes

A session is driven by two strictly separated planes (`hsm/src/observation.rs`):

| Plane | Direction | Transport | Purpose |
|-------|-----------|-----------|---------|
| **Inward (RTC)** | into the kernel | `mpsc` event queue via `EventSink` | One `Event` fully dispatched at a time; the source of truth |
| **Outward (observation)** | out of the kernel | `broadcast` channel via `ObservationSink` | Streaming `UiEvent`s for rendering: text deltas, tool progress, usage, transition trace |

The outward plane is **lossy by design** - a lagging subscriber observes a
`Lagged` error and skips dropped events, exactly like a render-tick stream. RTC
is preserved because streaming output never re-enters the inward queue: an
executor streams many `UiEvent`s while an effect is in flight, then posts
exactly one completion `Event` back inward.

`UiEvent` variants:

```
TextDelta / TextComplete            // streamed + final assistant text
ThinkingDelta / ThinkingComplete    // extended thinking
ToolStarted { call_id, name, args }
ToolProgress { call_id, message }
ToolFinished { call_id, name, output, is_error }
TokenUsage { input, output, cache_read, cache_write, ... , cost_usd }
ContextCompacted { tokens_before, tokens_after, strategy, turn }
TodoUpdate(Value)
ModeChanged(String) / ModelChanged(String)
Transition { from, to, event }      // full transition trace, emitted after every dispatch
Error(String)
TurnComplete
Aborted { partial_text }            // run aborted; carries streamed-but-uncommitted text
```

There is also a coarse `MachineProjection`-style snapshot published on a `watch`
channel after every dispatch (`RuntimeStatus`: `state_label`, `done`,
`last_error`, `processed`). Frontends render from these snapshots and from
`UiEvent`s; they never inspect internal machine state.

---

## Machines and the `ModeRegistry`

A `Machine` (`hsm/src/machine.rs`) describes a state hierarchy: its states
(`type State`), each state's `superstate`, and a single `dispatch_state`
handler. The kernel's `Hsm<M>` drives any `Machine` generically.

`ModeRegistry` (`machines/src/mode.rs`) maps a mode **string** to a machine
factory. This is the authoritative wiring; `RuntimeBuilder` looks up the machine
by the mode string and builds an `ErasedRuntime` around it.

| Mode string | Machine | Engine |
|-------------|---------|--------|
| `"agent"` | `ReactiveAgentMachine` | `TurnExecutor` + `loop_core` in-state tool loop |
| `"reactive"` | `ReactiveAgentMachine` | `TurnExecutor` + `loop_core` in-state tool loop |
| `"chat"` | `ReactiveAgentMachine` | `TurnExecutor` + `loop_core` in-state tool loop |
| `"sdlc"` | `SdlcMachine` | `TurnExecutor` + `loop_core`; per-phase in-state tool loops + structured JSON decisions |

All modes use `TurnExecutor` (`kind: "turn"`) as the single LLM call engine.
`SdlcMachine` routes each phase's decision parsing through its own `loop_core`
state handlers (parsing structured JSON from the final tool-free text).

Mode selection at startup (see `src/main.rs`) is, in priority order:

1. `SVEN_MODE` environment variable,
2. the `--mode` CLI flag (mapped into kernel vocabulary; e.g. `plan` → `sdlc`),
3. default `chat`.

### `ReactiveAgentMachine` (modes `agent` / `reactive` / `chat`)

The default streaming coding agent (`machines/src/machines/reactive_agent.rs`).
A turn-lifecycle machine driven by the shared `loop_core` state handlers:

```
Top
└── Session
    ├── Idle       ← waiting for the user's next message
    └── Generating ← LLM turn in flight; tool loop handled in-state
```

On a `UserMessage` in `Idle`, it emits one `Effect::CallLlm { kind: "turn" }` and
moves to `Generating`. `TurnExecutor` streams the model response and posts
`Event::LlmTurnComplete { text, tool_calls }` back. If `tool_calls` is non-empty
the machine emits one `Effect::CallTool` per call **and stays in `Generating`**
(via `Reaction::Handled`). Tool results arrive as `ToolSucceeded` / `ToolFailed`;
when all pending calls are settled the machine emits a continuation `CallLlm` and
stays in `Generating`. A final tool-call-free turn stores the response and
returns to `Idle`.

### `SdlcMachine` (mode `sdlc`)

The deliberation-driven software-development lifecycle machine
(`machines/src/machines/sdlc/`). Every phase is a *deliberation*: the state
issues one comprehensive instruction on its own append-only conversation thread
with a state-scoped tool subset, and the model returns a structured decision
whose `status` drives the transition.

```
Top
├── Idle          ← waits for the first UserMessage (the "hi" intake guard)
├── Intake        ← classify intent; chit-chat/clarify or confirm scope
├── Discovery     ← explore the repo (read-only tools)
├── Planning      ← produce + get approval for a plan
├── Execution     ← implement (write/build tools); may fan out per task
├── Verification  ← independent build/test verification
├── Delivery      ← summarise + final sign-off
├── Recovery      ← diagnose failures and retry/escalate
├── Done / Failed / Cancelled  ← terminal
```

**Each phase owns its tool loop in-state** - there are no separate
`RunningTools` or `AwaitingApproval` states. See
**[State Machine Reference](state-machines.md)** for the full transition tables.
The parallel fan-out used by `Execution` is documented in **[Parallel Submachine
Fan-out](parallel-submachines.md)**.

---

## Effect executors

Each executor implements `EffectExecutor` and performs the I/O for a subset of
effects, streaming `UiEvent`s outward and posting result `Event`s inward. The
`CompositeExecutor` (`executors/src/composite.rs`), built by
`RuntimeBuilder`, routes each effect to the right sub-executor.

| Executor | Effects handled |
|----------|-----------------|
| `TurnExecutor` | `CallLlm` with `kind: "turn"` - single-pass model streaming; accumulates tool proposals; posts `LlmTurnComplete` |
| `ToolExecutor` | `CallTool` - kernel-gated, spawn-and-forget; the **only** executor that calls `registry.execute` |
| `UserExecutor` | `AskUser`, `RequestHumanApproval`, `RequestHumanAnswer` |
| `TimerExecutor` | `ScheduleTimeout`, `CancelTimeout` |
| `AuditExecutor` | `PersistAudit` |
| `InternalExecutor` | `EmitInternal` |
| `VerifyExecutor` | `Verify` - jailed to the project root; posts `VerificationComplete` |

All `CallLlm` effects use `kind: "turn"` and are routed to `TurnExecutor`.
An effect is never dropped silently. When the permission gate refuses a
non-tool batch, or the `CompositeExecutor` has no slot for an effect (an
unrecognised `CallLlm` kind, an unconfigured executor), the machine is
answered with `sven_kernel::failure_event`: `LlmFailed` for `CallLlm`,
`ToolFailed` for `CallTool`, and `EffectFailed { kind, error }` for anything
else a machine may be waiting on (fire-and-forget effects - `PersistAudit`,
`EmitInternal`, `CancelTimeout` - get no answer). Every machine leaves a
waiting state on `EffectFailed` the way it does on `LlmFailed`.

`InstantiateSubmachine` is validated with the rest of its dispatch's
non-tool batch - it needs `SpawnChild` in the current state - and then handed
to the runtime's `ChildSpawner` (see below). A refusal is audited and
answered with `EffectFailed`, like any refused non-tool effect. Without a
spawner it reaches the `CompositeExecutor`, which answers it with
`EffectFailed` too.

---

## Submachine composition and parallel fan-out

The kernel supports two forms of composition:

- **In-process child (`Submachine<P>`, `hsm/src/submachine.rs`).** A parent
  holds an optional active child behind the object-safe `ErasedMachine` trait.
  While a child is active, events route to it first and bubble unhandled events
  to the parent; on child completion the parent receives
  `InternalEvent::SubmachineCompleted` and the child is dropped. This path is
  synchronous and runs the child inside the parent's dispatch.

- **Concurrent child kernels (`ChildSpawner` + `spawn_with_children`).** This is
  the production fan-out path. When a machine emits
  `Effect::InstantiateSubmachine`, the runtime validates it with the rest of the
  batch, records it in a lightweight child registry, and hands it to
  the installed `ChildSpawner`, which runs the child as its **own concurrent
  kernel with an isolated `Context`**. When a child finishes it posts
  `InternalEvent::SubmachineCompleted { machine, result }` back to the parent,
  carrying the child's structured result for append-only aggregation. The runtime
  drops the child from its registry on completion.

### Child runs

Every child run is started under a `ChildRunContract` (`hsm/src/contract.rs`):
the permission policy it runs with, optional `max_tool_rounds` and
`max_output_tokens` budgets, and an optional deadline. A child never holds
more than its parent:

- The kernel builds the contract from what the parent's policy allows in the
  state that emitted `InstantiateSubmachine` (`PermissionPolicy::ceiling_in`),
  narrowed by the parent's own contract when the parent is itself a child.
- `ChildRunContract::narrow` is the only way terms combine: a capability is
  allowed only where both sides allow it, an approval requirement from either
  side is kept, and each budget and the deadline take the tighter value. A
  spawner narrows the inherited contract with its own terms; it cannot widen
  it.
- An approval is for one call, so a child holds none of the parent's: under a
  policy that asks approval, each such call of the child is asked about
  itself.

The spawner receives the contract with a `CancelScope` derived from the
parent's (`ChildRun`). Cancelling the parent cancels every child, however
deep; the parent's loop ending for any reason - a terminal state, `cancel`,
`abort`, dropping the handle - cancels the children still live, because the
registry holding their scopes cancels them when dropped.
`ErasedRuntime::spawn_child_run` enforces what the kernel can see: the child
runs with the contract's policy, and its scope is cancelled at the contract's
deadline. The turn budgets belong to the child's own context and executor, and
the spawner applies them.

The full design, the `TaskMachine`, the `SdlcChildSpawner`, how a child
reaches the parent's human gates, and the Execution fan-out/aggregation flow
are in **[Parallel Submachine Fan-out](parallel-submachines.md)**.

---

## Conversation primitives

- **`ThreadStore`** (`sven-executors`, `thread_store.rs`) - append-only
  per-thread `Vec<Message>` history with a cache-safety invariant. Each
  thread's prefix is immutable; new messages are only ever appended. This
  keeps provider prompt caches valid across successive turns on the same
  thread.
- **`TurnRequest`** (`sven-vocab`, `turn.rs`) - the `kind: "turn"` request
  shape every machine uses for a single model pass. Fields include `thread`,
  `tools`, `all_tools_mode`, `model`, and `instruction`. It is pure data, so a
  machine builds one without depending on the model or executor layers.

The model layer (`sven-model`) carries the streaming primitives the engines
share: `CompletionRequest` (now including an optional `response_format` for
structured output), `Message` / `MessageContent` (text, multimodal parts, tool
calls, tool results), `ResponseEvent` (the streamed `TextDelta` /
`ThinkingDelta` / `ToolCall` / `Usage` / `Done` / `MaxTokens` events), and
`ResponseFormat` (`JsonObject` or `JsonSchema { name, schema }`).

---

## Tools (`sven-tool-api`, `sven-tool-registry`)

`ToolRegistry` (`tool-registry/src/registry.rs`) holds all available tools behind a
`RwLock` (so MCP tools can be swapped at runtime). Beyond execution it provides
the **tool-subset API** the `SdlcMachine` relies on:

- `schemas()` / `schemas_for_mode(mode)` - all tools, or those for a mode.
- `schemas_for_names(&[String])` - only the named subset (unknown names are
  silently skipped), used to give each SDLC state a *state-scoped* tool set.
- `known_names(&[String])` - which requested names are actually registered.

Schemas are always ordered core-tools-first (sorted), then MCP tools (sorted),
to keep provider cache breakpoints stable. `execute` honours an optional
`PermissionRequester` so tools whose policy is `Ask` are gated by an IDE/ACP
`request_permission` round-trip before running.

---

## Multi-session supervisor and `RuntimeBuilder`

`RuntimeBuilder` (`bootstrap/src/runtime_builder.rs`) is the per-session
factory. It looks up the machine from the `ModeRegistry`, builds the model
provider, tool registry, and MCP manager, assembles the `CompositeExecutor`
(wiring `TurnExecutor` for all modes; for `sdlc` also installs `SdlcChildSpawner`),
and spawns an `ErasedRuntime` via `spawn_with_children`. For `sdlc` it seeds the
`parallel_execution` fact so the machine only fans out when a spawner is actually
installed.

```rust,ignore
let bundle = RuntimeBuilder::new(config, "sdlc")
    .with_runtime_context(ctx)
    .with_tool_question_tx(question_tx)   // TUI question modal
    .with_permission_requester(perm)      // ACP IDE approval
    .with_initial_history(messages)       // resume a session
    .build_session()
    .await?;
```

`build_session` returns a `SessionBundle` holding the `ErasedRuntime`, a cheap
`RuntimeHandle` (sink + observation + status), and the `KernelChannels`
(question/approval receivers). All modes drive the model through `TurnExecutor`. A
`SessionSupervisor` manages a registry of such bundles keyed by `SessionId`,
sharing reference-counted skills and knowledge across sessions.

---

## UI integration

The TUI is **not** part of the machine - it is a projection consumer and an
event source:

- **Event source**: keystrokes, approval decisions, and user text are posted to
  the kernel queue as typed `Event`s via `EventSink`.
- **Projection consumer**: the TUI subscribes to the observation `UiEvent`
  stream and renders the coarse `RuntimeStatus` snapshot; it never inspects
  internal machine state.

---

## CI / headless mode

`RuntimeRunner` (`ci/src/runner/runtime_runner.rs`) drives the kernel with
no UI. It builds a `SessionBundle` (mapping the caller's mode to a registered
kernel mode - coding/plan/research all resolve to `agent`, `chat`→`chat`,
`sdlc`→`sdlc`), **auto-approves every human gate** (questions get an empty
answer, approvals get `true`), posts the prompt as `Event::UserMessage`, bridges
each `UiEvent` to stdout/stderr, and returns exit code `0` on `TurnComplete`,
non-zero on error or timeout. The CI auto-approve behaviour preserves the same
human-gate flow as interactive mode without blocking.

---

## Testing

Because transition functions are pure, unit tests construct an event and assert
on the resulting `(new_state, effects)`:

```rust,ignore
let mut hsm = Hsm::new(ReactiveAgentMachine::new());
let mut ctx = Context::new();
hsm.init(&mut ctx);
let out = hsm.dispatch(&Event::user_message("fix the bug"), &mut ctx);
assert_eq!(hsm.state(), ReactiveState::Generating);
assert!(out.effects.iter().any(|e| matches!(e, Effect::CallLlm { .. })));
```

Integration tests replay an event log and assert on the final state.
The `TurnExecutor` is tested with scripted `ModelProvider`s, and
E2E bats tests use `--model mock` so no real API key is required.

---

## Crate map

| Crate | Role |
|-------|------|
| `sven-hsm` | HSM kernel vocabulary and pure dispatch: `Machine` trait, `Hsm`, permissions, audit/replay, snapshots, `Submachine`, `ObservationSink`/`UiEvent` |
| `sven-kernel` | Active Object runtime: `Runtime`/`ErasedRuntime`, `EffectExecutor`, `EventSink`, `ChildSpawner`, `Clock`/timers |
| `sven-model` | Stateless provider abstraction: `ModelProvider`, `CompletionRequest` (incl. `response_format`), `Message`, `ResponseEvent`, `ResponseFormat` |
| `sven-tool-api` | `Tool` trait, `ToolCall` / `ToolOutput`, approval policy / `PermissionRequester`, tool events and display |
| `sven-tool-registry` | `ToolRegistry` (incl. tool-subset API), `ToolSchema`, `SharedTools`, `ToolPolicy` |
| `sven-tools-*` | Concrete tool implementations by domain: `fs`, `exec`, `web`, `ctx`, `agent`, `gdb`, `android` |
| `sven-machines` | Concrete machines (`ReactiveAgentMachine`, `SdlcMachine` + `TaskMachine`, `VerifiedTaskMachine`, `UiTestMachine`), `loop_core` shared state handlers, `ModeRegistry`. Depends only on `sven-hsm` and `sven-vocab` |
| `sven-turn` | Impure turn primitives: `stream_turn`, compaction, system-prompt assembly, `AgentRuntimeContext` |
| `sven-executors` | Effect executors: `TurnExecutor`, tool, user, timer, audit, internal, verify, and the `CompositeExecutor` router |
| `sven-bootstrap` | `RuntimeBuilder` (per-session factory), `SessionSupervisor`, `SdlcChildSpawner` |
| `sven-frontend` | Shared agent wiring for frontends: the kernel session task, request queue, tool-call view and markdown block parser |
| `sven-ci` | `RuntimeRunner` - headless kernel driver for batch/CI runs |
| `sven-acp` | ACP server backed by a per-session kernel |

---

## Further reading

- **[State Machine Reference](state-machines.md)** - complete states, events,
  effects, transition tables, loop-core API, and `LoopState` struct for every
  machine in sven. Start here for machine-level details.
- **[Parallel Submachine Fan-out](parallel-submachines.md)** - `ChildSpawner`,
  isolated child kernels, `TaskMachine`, `SdlcChildSpawner`, and the Execution
  fan-out/aggregation flow.
- Miro Samek, *Practical UML Statecharts in C/C++, 2nd ed.* - the dispatch
  algorithm in `hsm/src/dispatch.rs` follows its two-phase design.
- [sven-hsm tests](../../crates/hsm/tests/) - LCA ordering, permission
  rejection and replay equality; [sven-kernel tests](../../crates/kernel/tests/) -
  virtual-time timeouts and child-spawner fan-out.
- [SdlcMachine source](../../crates/machines/src/machines/sdlc/) ·
  [ReactiveAgentMachine source](../../crates/machines/src/machines/reactive_agent.rs)
