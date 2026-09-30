# State Machine Reference

This document is the complete reference for every state machine in sven: their
states, the events each state handles, the effects each transition emits, and
how the three machines relate to each other. Read this before reading the
source.

---

## Overview

Sven has three concrete machines, all sharing a common [loop-core](#the-shared-loop-core).

| Machine | Mode(s) | Role |
|---------|---------|------|
| [`ReactiveAgentMachine`](#reactiveagentmachine) | `chat`, `agent`, `reactive` | Streaming coding assistant; one turn per user message |
| [`SdlcMachine`](#sdlcmachine) | `sdlc` | Full software-development lifecycle; drives many turns autonomously |
| [`TaskMachine`](#taskmachine) | child of `SdlcMachine` | One-shot parallel task worker fanned out by `SdlcMachine` during `Execution` |

Every machine obeys the same rules:

- A machine **never performs I/O**. It receives an `Event`, optionally mutates
  its `Context`, and returns a `Reaction` that contains zero or more `Effect`s.
- The kernel **validates** every `Effect` against the machine's `PermissionPolicy`,
  executes allowed effects on separate tasks, and posts result `Event`s back into
  the queue.
- All three machines share the **loop-core** helpers for the in-state model↔tool
  loop (no separate `RunningTools` or `AwaitingApproval` states).

---

## Event Vocabulary

Events are the **sole input** to a machine. Nothing reaches into the machine
directly; every external notification becomes a typed `Event` first.

### Human events

| Event | Payload | Meaning |
|-------|---------|---------|
| `UserMessage` | `text: String` | A human sent a chat or instruction message |
| `UserProvidedArtifact` | `artifact: Value` | A human attached a file, image, or blob |
| `UserCancelled` | - | The human asked to cancel in-flight work |

### LLM events

| Event | Payload | Meaning |
|-------|---------|---------|
| `LlmTurnComplete` | `thread, text, tool_calls: Vec<ProposedToolCall>` | A `TurnExecutor` streaming pass finished; the machine inspects `tool_calls` to decide its next action |
| `LlmFailed` | `error: String` | The model failed to produce a usable result |

### Tool result events

| Event | Payload | Meaning |
|-------|---------|---------|
| `ToolSucceeded` | `call_id, observation: Value` | A kernel-dispatched `CallTool` completed successfully |
| `ToolFailed` | `call_id, error: String` | A kernel-dispatched `CallTool` failed or was denied |
| `ToolApprovalRequired` | `call_id, capability, description` | The permission gate requires human approval before the tool can run |

### Human gate events

| Event | Payload | Meaning |
|-------|---------|---------|
| `HumanApproved` | `approval_id` | A human approved a pending `RequestHumanApproval` |
| `HumanRejected` | `approval_id` | A human rejected a pending `RequestHumanApproval` |

### Timer and internal events

| Event | Payload | Meaning |
|-------|---------|---------|
| `Timeout` | `timer_id` | A scheduled timer elapsed |
| `Internal(Entry)` | - | Reserved lifecycle signal: state is being entered |
| `Internal(Exit)` | - | Reserved lifecycle signal: state is being exited |
| `Internal(Init)` | - | Reserved lifecycle signal: fire the composite state's initial transition |
| `Internal(SubmachineCompleted)` | `machine, result: Value` | A child submachine reached its terminal state; `result` carries its output |
| `Internal(Custom)` | `name, payload` | A domain-defined internal signal |

---

## Effect Vocabulary

Effects are **pure data** returned by the machine. No machine ever calls I/O
directly; it asks the kernel to do it by returning an effect.

| Effect | Payload | Meaning |
|--------|---------|---------|
| `CallLlm` | `request: Value` | Ask the LLM service for a response; result arrives as `LlmTurnComplete` or `LlmFailed` |
| `CallTool` | `call_id, name, capability, args` | Invoke a tool; result arrives as `ToolSucceeded` or `ToolFailed` |
| `AskUser` | `prompt: String` | Ask the human a question; answer arrives as `UserMessage` |
| `RequestHumanApproval` | `approval_id, capability, description` | Gate a dangerous capability on human consent; answer arrives as `HumanApproved` or `HumanRejected` |
| `ScheduleTimeout` | `timer_id, duration` | Post `Timeout { timer_id }` after `duration` |
| `CancelTimeout` | `timer_id` | Cancel a previously scheduled timer |
| `PersistAudit` | - | Flush the in-memory audit log to durable storage |
| `EmitInternal` | `name, payload` | Re-enter a domain signal into the event queue |
| `InstantiateSubmachine` | `machine: MachineId, descriptor: Value` | Create a child submachine and route events to it |

### `CallLlm` request kind

All `CallLlm` effects use `kind = "turn"`, handled by `TurnExecutor`:

- Streams one model response from the configured LLM provider.
- Appends the assistant turn (text + tool calls) to the `ThreadStore` thread.
- Posts `LlmTurnComplete { thread, text, tool_calls }` back into the kernel queue.
- The machine then decides purely based on `tool_calls` and decision text what to
  do next - no I/O happens inside the machine.

---

## Tool Capability Buckets

Every `CallTool` effect carries a `ToolCapability` that the `PermissionPolicy`
checks against what the machine's current state is allowed to do.

| Capability | Tools that use it | Dangerous? |
|------------|-------------------|------------|
| `ReadFile` | `read_file`, `find_file`, `grep`, `context_*`, `buf_*`, `list_*`, `search_*` | No |
| `WriteFile` | `write_file`, `edit_file` | No |
| `DeleteFile` | none (no built-in tool uses it) | Yes (requires approval) |
| `ExecuteShell` | `shell`, `gdb_*` | Yes (requires approval) |
| `NetworkAccess` | `web_fetch`, `web_search`, MCP tools | No |
| `GitOperation` | `git_*` | No |

Capabilities marked **Dangerous** always require an explicit `HumanApproved`
event before the kernel dispatches them, regardless of the per-state allow-set.

---

## ReactiveAgentMachine

Used in `--mode chat`, `--mode agent`, `--mode reactive`.

### State Hierarchy

```
Top (root, never active leaf)
└── Session (composite)
    ├── Idle       ← waiting for the user's next message
    └── Generating ← LLM turn in flight; tools executed in-state
```

`Generating` owns the full tool loop via in-state handling - there are no
separate `RunningTools` or `AwaitingApproval` states.

### Permission Policy

| Capability | Allowed in all states | Requires approval |
|------------|-----------------------|-------------------|
| `ReadFile` | ✓ | - |
| `WriteFile` | ✓ | - |
| `NetworkAccess` | ✓ | - |
| `GitOperation` | ✓ | - |
| `ExecuteShell` | ✓ | ✓ (inherently dangerous) |

### State Transitions

#### `Idle`

| Event | Guard | → State | Effects emitted |
|-------|-------|---------|-----------------|
| `UserMessage { text }` | - | `Generating` | `CallLlm { kind:"turn", thread:"chat", instruction:text }` |
| _(any other)_ | - | `Session` (super) | - |

#### `Session` (composite parent of Idle/Generating)

| Event | → State | Effects emitted |
|-------|---------|-----------------|
| `Internal(Init)` | `Idle` | - |
| `UserCancelled` | `Idle` | - |
| _(any other)_ | `Top` (super) | - |

#### `Generating`

Tool events are handled **in-state** (`Reaction::Handled`) - no state transition
occurs while tools are running.

| Event | Condition | → State | Effects emitted |
|-------|-----------|---------|-----------------|
| `LlmTurnComplete { tool_calls: [] }` | text non-empty | `Idle` | - _(stores `last_response`)_ |
| `LlmTurnComplete { tool_calls: [] }` | text empty | _(stay)_ | `CallLlm` (nudge turn) |
| `LlmTurnComplete { tool_calls: [..] }` | rounds ≤ max | _(stay)_ | `CallTool` × N (one per call) |
| `LlmTurnComplete { tool_calls: [..] }` | rounds > max | _(stay)_ | `CallLlm` (wrap-up instruction) |
| `ToolSucceeded` or `ToolFailed` | pending calls remain | _(stay)_ | - |
| `ToolSucceeded` or `ToolFailed` | last pending call | _(stay)_ | `CallLlm { kind:"turn" }` (continuation) |
| `ToolApprovalRequired` | - | _(stay)_ | `RequestHumanApproval` |
| `HumanApproved` / `HumanRejected` | approval was for a tool | _(stay)_ | `CallLlm { kind:"turn" }` (resume) |
| `LlmFailed { error }` | - | `Idle` | - _(stores `last_error`)_ |
| _(any other)_ | - | `Session` (super) | - |

### Full Round-Trip Trace (single tool call)

```
UserMessage("fix the bug")
  → [Idle] → Generating + CallLlm(kind="turn", thread="chat", instruction="fix the bug")
  ← LlmTurnComplete(thread="chat", text="", tool_calls=[{read_file, ...}])
  → [Generating/in-state] + CallTool(call_id=A, name="read_file", cap=ReadFile)
  ← ToolSucceeded(call_id=A, observation="…file contents…")
  → [Generating/in-state] + CallLlm(kind="turn", thread="chat")
  ← LlmTurnComplete(thread="chat", text="The bug is on line 42…", tool_calls=[])
  → [Generating] → Idle
```

---

## SdlcMachine

Used in `--mode sdlc`.

### State Hierarchy

```
Top (root, never active leaf)
├── Idle          ← waits for the first UserMessage
├── Intake        ← classify user intent; decide scope
├── Discovery     ← read-only exploration of the codebase
├── Planning      ← produce an implementation plan
├── Execution     ← implement the plan (write/build/commit)
├── Verification  ← independent build + test run
├── Delivery      ← summarise, sign off, notify user
├── Recovery      ← diagnose failures; retry or escalate
├── Done          ← terminal: work delivered successfully
├── Failed        ← terminal: unrecoverable error
└── Cancelled     ← terminal: user cancelled
```

All states are flat children of `Top`. **Each phase owns its tool loop** via
in-state handling - there are no shared `RunningTools` or `AwaitingApproval`
states. This means `CallTool` effects are always permission-gated against the
*real* phase state (e.g. `Execution`), not a generic shared state.

### Permission Policy (per state)

| Capability | `Idle` | `Intake` | `Discovery` | `Planning` | `Execution` | `Verification` | `Delivery` | `Recovery` |
|------------|--------|----------|-------------|------------|-------------|----------------|------------|------------|
| `ReadFile` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `GitOperation` | - | - | ✓ | ✓ | ✓ | - | ✓ | - |
| `WriteFile` | - | - | - | - | ✓ | - | - | - |
| `ExecuteShell` | - | - | - | - | ✓ | ✓ | - | - |
| `NetworkAccess` | - | - | - | - | - | - | - | - |
| `DeleteFile` | - | - | - | - | - | - | - | - |

### Conversation Threads and Tool Subsets (per phase)

Each SDLC phase runs on its own **append-only conversation thread** and is
given a focused **tool subset** to prevent scope creep. The LLM response schema
is always the structured `decision` object.

| Phase state | Thread | Tool subset |
|-------------|--------|-------------|
| `Intake` | `"intake"` | read-only (`read_file`, `grep`, `find_file`, `context_*`) |
| `Discovery` | `"discovery"` | read-only + git info (`git_log`, `git_diff`, …) |
| `Planning` | `"planning"` | read-only |
| `Execution` | `"execution"` | read + write + shell (`edit_file`, `write_file`, `shell`, `git_*`) |
| `Verification` | `"verification"` | shell only (`shell`) |
| `Delivery` | `"delivery"` | git (`git_commit`, `git_push`) |
| `Recovery` | `"recovery"` | read-only + diagnostics |
| `TaskMachine` | `"task"` | write tools (same as Execution) |

### Decision Schema

Every SDLC phase instructs the LLM to respond with a JSON object matching:

```json
{
  "status": "proceed | need_user_input | need_approval | need_tools | failed",
  "summary": "one-sentence summary",
  "message": "human-readable message or question",
  "questions": ["optional list of specific questions"],
  "approval_prompt": "optional approval request text",
  "payload": { ... }
}
```

The machine maps `status` to a state transition:

| `status` | Transition |
|----------|------------|
| `proceed` | advance to the next phase |
| `need_user_input` | emit `AskUser`; stay in current phase waiting for `UserMessage` |
| `need_approval` | emit `RequestHumanApproval`; stay waiting for `HumanApproved` |
| `need_tools` | emit another `CallLlm` continuation turn |
| `failed` | transition to `Recovery` |

### State Transitions

#### `Idle`

| Event | → State | Effects emitted |
|-------|---------|-----------------|
| `UserMessage { text }` | `Intake` | `CallLlm { kind:"turn", thread:"intake", instruction:text }` |
| _(any other)_ | _(ignored)_ | - |

#### Standard phases: `Intake`, `Discovery`, `Planning`, `Verification`, `Delivery`

Each of these phases handles the tool loop in-state (`Reaction::Handled` for tool events):

| Event | `decision.status` | → State | Effects emitted |
|-------|-------------------|---------|-----------------|
| `Internal(Entry)` | - | _(stay)_ | `CallLlm { kind:"turn", thread:"<phase>" }` |
| `LlmTurnComplete { tool_calls:[] }` | `proceed` | _next phase_ | - |
| `LlmTurnComplete { tool_calls:[] }` | `need_user_input` | _(stay)_ | `AskUser { prompt }` |
| `LlmTurnComplete { tool_calls:[] }` | `need_approval` | _(stay)_ | `RequestHumanApproval` |
| `LlmTurnComplete { tool_calls:[] }` | `need_tools` | _(stay)_ | `CallLlm` (continuation) |
| `LlmTurnComplete { tool_calls:[] }` | `failed` | `Recovery` | - |
| `LlmTurnComplete { tool_calls:[..] }` | - | _(stay)_ | `CallTool` × N |
| `ToolSucceeded` or `ToolFailed` | pending calls remain | _(stay)_ | - |
| `ToolSucceeded` or `ToolFailed` | last call done | _(stay)_ | `CallLlm { kind:"turn" }` (continuation) |
| `ToolApprovalRequired` | - | _(stay)_ | `RequestHumanApproval` |
| `HumanApproved` | approval was for a tool | _(stay)_ | `CallLlm { kind:"turn" }` (resume) |
| `HumanApproved` | decision-level approval | _next phase_ | `CallLlm { kind:"turn" }` (next phase) |
| `HumanRejected` | - | _(stay)_ | `AskUser` or `CallLlm` revision |
| `UserMessage { text }` | - | _(stay)_ | `CallLlm { kind:"turn", instruction:text }` |
| `LlmFailed` | - | `Recovery` | - |

Phase-specific next-phase mappings:

| Current phase | `proceed` target | `HumanApproved` (decision) target |
|---------------|------------------|------------------------------------|
| `Intake` | `Discovery` | `Discovery` |
| `Discovery` | `Planning` | `Planning` |
| `Planning` | `Execution` | `Execution` |
| `Verification` | `Delivery` | `Delivery` |
| `Delivery` | `Done` | `Done` |

#### `Execution`

Implements the plan. Has access to write and shell tools. Additionally handles
parallel child submachines:

| Event | Condition | → State | Effects emitted |
|-------|-----------|---------|-----------------|
| `Internal(Entry)` | `parallel_execution` + tasks | _(stay)_ | `InstantiateSubmachine` × N |
| `Internal(Entry)` | single-track | _(stay)_ | `CallLlm { kind:"turn", thread:"execution" }` |
| `LlmTurnComplete` | _(standard tool loop as above)_ | see above | see above |
| `Internal(SubmachineCompleted)` | more children pending | _(stay)_ | - |
| `Internal(SubmachineCompleted)` | last child done | _(stay)_ | `CallLlm { kind:"turn", thread:"execution" }` with child digests |
| `LlmTurnComplete { tool_calls:[] }` | `proceed` | `Verification` | - |
| `LlmFailed` | - | `Recovery` | - |

#### `Recovery`

Diagnoses the failure from the previous phase and decides whether to retry.

| Event | `decision.status` | → State | Effects emitted |
|-------|-------------------|---------|-----------------|
| `Internal(Entry)` | - | _(stay)_ | `CallLlm { kind:"turn", thread:"recovery" }` |
| `LlmTurnComplete { tool_calls:[] }` | `proceed` | _(the failed phase)_ | - |
| `LlmTurnComplete { tool_calls:[] }` | `need_user_input` | _(stay)_ | `AskUser` |
| `LlmTurnComplete { tool_calls:[] }` | other | `Failed` | - |
| `LlmTurnComplete { tool_calls:[..] }` | - | _(stay)_ | `CallTool` × N |
| `LlmFailed` | - | `Failed` | - |

Recovery tracks an attempt counter in `ctx["recovery_count"]`; after
`MAX_RECOVERY` (3) attempts without `proceed`, it transitions to `Failed`.

#### Terminal states

| State | Meaning |
|-------|---------|
| `Done` | Delivery accepted; machine stops |
| `Failed` | Unrecoverable error; machine stops |
| `Cancelled` | `UserCancelled` received; machine stops |

### Full SDLC Round-Trip Trace

```
UserMessage("add JSON logging")
  → [Idle] → Intake + CallLlm(kind="turn", thread="intake", instruction="add JSON logging")
  ← LlmTurnComplete(text="", tool_calls=[{read_file, "Cargo.toml"}])
  → [Intake/in-state] + CallTool(call_id=A, name="read_file", cap=ReadFile)
  ← ToolSucceeded(call_id=A, observation="[package]\n…")
  → [Intake/in-state] + CallLlm(kind="turn", thread="intake")
  ← LlmTurnComplete(text='{"status":"proceed","summary":"Add tracing-subscriber…"}')
  → [Intake] → Discovery + CallLlm(kind="turn", thread="discovery")
  ← LlmTurnComplete(text='{"status":"proceed","summary":"Found 3 affected modules"}')
  → [Discovery] → Planning + CallLlm(kind="turn", thread="planning")
  ← LlmTurnComplete(text='{"status":"need_approval","approval_prompt":"Create feature branch?"}')
  → [Planning/in-state] + RequestHumanApproval(cap=GitOperation)
  ← HumanApproved
  → [Planning] → Execution + CallLlm(kind="turn", thread="execution")
  … (write/edit tool rounds handled in-state in Execution) …
  → [Execution] → Verification + CallLlm(kind="turn", thread="verification")
  ← LlmTurnComplete(text='{"status":"proceed","summary":"All tests pass"}')
  → [Verification] → Delivery + CallLlm(kind="turn", thread="delivery")
  ← LlmTurnComplete(text='{"status":"proceed","summary":"PR opened #42"}')
  → [Delivery] → Done
```

---

## TaskMachine

A one-shot child submachine fanned out by `SdlcMachine` during the `Planning`
or `Execution` phase to implement a single decomposed task in parallel.

### State Hierarchy

```
Top (root)
├── Run  ← executes the task; tool loop handled in-state
└── Done ← terminal; result stored in ctx["out"]
```

`Run` handles the full tool loop in-state (same as `Generating` in
`ReactiveAgentMachine`) - no separate `RunningTools` state.

### Permission Policy

Inherits the parent `SdlcMachine`'s `Execution` policy (write + shell tools).
Child kernels run with a tightened policy; dangerous capabilities resolve as
`ToolFailed` rather than prompting the user.

### State Transitions

#### `Run`

| Event | Condition | → State | Effects emitted |
|-------|-----------|---------|-----------------|
| `Internal(Entry)` | fresh entry | _(stay)_ | `CallLlm { kind:"turn", thread:"task" }` |
| `LlmTurnComplete { tool_calls:[] }` | - | `Done` | - _(parses decision; stores `ctx["out"]`)_ |
| `LlmTurnComplete { tool_calls:[..] }` | rounds ≤ max | _(stay)_ | `CallTool` × N |
| `LlmTurnComplete { tool_calls:[..] }` | rounds > max | `Done` | - _(stores failure result)_ |
| `ToolSucceeded` or `ToolFailed` | pending calls remain | _(stay)_ | - |
| `ToolSucceeded` or `ToolFailed` | last call done | _(stay)_ | `CallLlm { kind:"turn", thread:"task" }` (continuation) |
| `ToolApprovalRequired` | - | _(stay)_ | - _(auto-denied; synthesised as `ToolFailed`)_ |
| `LlmFailed` | - | `Done` | - _(stores failure result)_ |

#### Result format (`ctx["out"]`)

```json
{
  "task":    "implement the read_file tool",
  "summary": "Added read_file.rs and registered it in the registry",
  "ok":      true,
  "payload": { ... }
}
```

The parent `SdlcMachine` harvests `result` from the `SubmachineCompleted` event
and injects a digest into the `execution` thread before its next `CallLlm`.

---

## The Shared Loop Core

All three machines compose `machines::loop_core` instead of duplicating the
model↔tool loop logic.

### What loop_core provides

| Helper | Purpose |
|--------|---------|
| `init_loop(ctx, thread, tools, mode, max_rounds)` | Initialise `LoopState` in context when entering a generating state |
| `build_turn_effect(thread, tools, mode, model, instruction, …)` | Build the canonical `CallLlm { kind:"turn" }` effect |
| `on_llm_turn_complete(ctx, event) → GeneratingAction` | Classify a `LlmTurnComplete` as `FinalAnswer / CallTools / EmptyTurn / MaxRoundsReached` |
| `handle_tool_event(ctx, make_turn, event) → Option<Reaction<S>>` | Handle `ToolSucceeded/Failed/ApprovalRequired` and `HumanApproved/Rejected` in-state; returns `None` for non-tool events |

### `LoopState` struct

A single typed struct stored under `"lc_state"` in the `Context` replaces
six separate JSON facts. It holds:

| Field | Type | Description |
|-------|------|-------------|
| `thread` | `String` | Active conversation thread ID |
| `tools` | `Vec<String>` | Allowed tool names for the current loop |
| `all_tools_mode` | `String` | Mode string passed to `TurnExecutor` when `tools` is empty |
| `round` | `u32` | Current tool-call round counter |
| `max_rounds` | `u32` | Configured maximum rounds before wrap-up nudge |
| `pending` | `HashSet<ToolCallId>` | UUIDs of in-flight `CallTool` effects |
| `awaiting_tool_approval` | `Option<ApprovalId>` | Set when a tool (not a decision) approval is pending; distinguishes tool-gate from decision-gate `HumanApproved` |

### `GeneratingAction` variants

`on_llm_turn_complete` returns one of:

| Variant | Condition | Machine action |
|---------|-----------|----------------|
| `FinalAnswer { thread, text }` | no tool calls, non-empty text | transition to `Idle` / `Done` |
| `CallTools { thread, tool_effects, calls }` | tool calls present, rounds ≤ max | emit `CallTool` effects; stay in current state |
| `EmptyTurn { nudge_effect }` | no tool calls, empty text | emit nudge `CallLlm`; stay in current state |
| `MaxRoundsReached { wrapup_effect }` | tool calls present, rounds > max | emit wrap-up `CallLlm`; stay in current state |

---

## How the Machines Relate

```
CLI / TUI
   │  UserMessage
   ▼
KernelRuntime  ──────────────────────────────────────────────┐
   │                                                         │
   │  dispatches Event to active machine                     │
   ▼                                                         │
ReactiveAgentMachine          SdlcMachine                    │
  Top → Session                Top (flat)                    │
    Idle                         Idle                        │
    Generating ──────────────►   Intake                      │
    (tool loop in-state)         Discovery                    │
                                 Planning ──InstantiateSubmachine──►
                                 Execution                   │
                                 Verification           TaskMachine (child kernel)
                                 Delivery                  Top → Run
                                 Recovery                  (tool loop in-state)
                                 Done/Failed/Cancelled        Done
   │
   │  returns Vec<Effect>
   ▼
PermissionPolicy (per-call classify for CallTool)
   │
   ├── Allowed → ToolExecutor → ToolSucceeded / ToolFailed
   ├── NeedsApproval → ToolApprovalRequired event → in-state approval gate
   └── Forbidden → ToolFailed (synthesised inline; never dispatched)
```

**`TurnExecutor`** is the only component that calls the LLM. It streams a
single model response, accumulates tool-call JSON slot-by-slot (without
executing any tool), appends the assistant turn to the `ThreadStore`
thread, and posts `LlmTurnComplete`. The machine then decides what to do next
as a pure state transition.

**`ToolExecutor`** is the only component that calls tools. It dispatches
`CallTool` effects in parallel (spawn-and-forget), appends results to the
`ThreadStore`, and posts `ToolSucceeded` / `ToolFailed`. No executor
ever calls another executor.
