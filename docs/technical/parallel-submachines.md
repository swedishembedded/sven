# Parallel Submachine Fan-out

When the SDLC machine's approved plan decomposes into several independent tasks,
sven runs them **concurrently** - each task in its own isolated child kernel -
and merges the results back into the parent. This document describes the kernel
machinery that makes that possible (`ChildSpawner`, the child registry,
`spawn_with_children`, isolated child contexts, and the `SubmachineCompleted`
result payload), the one-shot `TaskMachine`, the production `SdlcChildSpawner`,
the Execution fan-out/aggregation flow, and an honest statement of the current
limitation around child user-gates.

For the kernel basics this builds on, see [HSM Architecture](hsm-architecture.md);
for the SDLC phases that drive it, see [Deliberation Engine](deliberation-engine.md).

---

## Why concurrent child kernels

The kernel already supports an *in-process* child (`Submachine<P>`), but that
runs the child synchronously inside the parent's dispatch - fine for a single
nested workflow, wrong for parallelism. To run N tasks at once without blocking
the parent's single run-to-completion consumer loop, each task gets its **own
`Runtime`**: its own event queue, its own consumer task, and its own `Context`
and conversation store. The parent stays responsive and the tasks make progress
simultaneously.

`Effect::InstantiateSubmachine` was previously a no-op. It is now genuinely
implemented at the runtime layer, and `InternalEvent::SubmachineCompleted`
carries a `result` payload so a finished child can hand its structured output
back to the parent for append-only aggregation.

---

## Kernel building blocks

### `ChildSpawner`

The kernel is machine-agnostic, so it cannot build a concrete child from an
opaque descriptor. The `ChildSpawner` trait (`hsm/src/runtime.rs`) bridges
that gap:

```rust,ignore
#[async_trait]
pub trait ChildSpawner: Send + Sync {
    async fn spawn_child(&self, machine: MachineId, descriptor: Value, parent: EventSink);
}
```

Given the parent-assigned `MachineId`, the opaque `descriptor` (which names the
work), and a clone of the parent's `EventSink`, an implementation must run the
child **concurrently on its own task with an isolated `Context`** and, when the
child reaches a terminal state, post
`Event::Internal(InternalEvent::SubmachineCompleted { machine, result })` back to
the parent. `spawn_child` must return promptly (spawn-and-forget); the long child
work belongs on the task it spawns, never inline - that is what keeps fan-out
parallel.

### `spawn_with_children` and the child registry

Both `Runtime<M>` and `ErasedRuntime` expose `spawn_with_children`, which takes an
optional `Arc<dyn ChildSpawner>`. Inside the consumer loop:

- After each dispatch, `spawn_children` peels every `Effect::InstantiateSubmachine`
  out of the effect batch, records it in a lightweight **child registry**
  (`HashMap<MachineId, ()>`), and calls `spawner.spawn_child(...)`. The remaining
  (non-child) effects continue down the normal validate-then-execute path.
- Before each dispatch, `note_child_completion` removes a child from the registry
  when a `SubmachineCompleted` event arrives, keeping an authoritative
  concurrent-child count for observability and shutdown.

If **no** spawner is configured, `InstantiateSubmachine` effects are left in
place and flow to the `EffectExecutor` - preserving the historical no-op so every
existing caller behaves unchanged. (The `CompositeExecutor` then merely logs a
warning for them.) Aggregation itself lives in the *parent machine*, which owns
the append-only thread; the kernel only tracks liveness.

### Isolated child `Context` and the `result` payload

Each child runs with a **fresh `Context`** - its own facts, conversation store,
audit trail, and retry counters - so children never share mutable state. The
child's terminal output travels back to the parent solely through the `result`
field of `SubmachineCompleted` (which defaults to `Null` for children that
produce no structured result). The parent consumes that payload and appends a
synthesis turn to its own thread; it never reaches into a child's state.

```mermaid
flowchart TD
    subgraph Parent["Parent kernel (SdlcMachine, Execution state)"]
      P1[Entry: plan has N tasks<br/>and parallel_execution set] --> P2[emit N x InstantiateSubmachine]
    end
    P2 -->|runtime peels effects| R[spawn_children:<br/>register + call spawner]
    R --> S[SdlcChildSpawner.spawn_child<br/> one per task]
    S --> C1[Child kernel 1<br/>fresh Context + store<br/>TaskMachine]
    S --> C2[Child kernel 2<br/>fresh Context + store<br/>TaskMachine]
    S --> C3[Child kernel N<br/>fresh Context + store<br/>TaskMachine]
    C1 -->|SubmachineCompleted result| AGG[Parent Execution:<br/>collect results,<br/>decrement exec_remaining]
    C2 -->|SubmachineCompleted result| AGG
    C3 -->|SubmachineCompleted result| AGG
    AGG --> D{remaining == 0?}
    D -- no --> AGG
    D -- yes --> M[Append merged digest<br/>to execution thread append-only] --> RD[Re-deliberate execution<br/>integrate + confirm]
```

---

## The `TaskMachine`

`TaskMachine` (`core/src/machines/sdlc/task.rs`) is the one-shot child the
SDLC parent fans out to. It uses the `loop_core` state handlers to run a
kernel-mediated multi-round turn loop for a single task on its own isolated
`task` conversation thread, then completes:

```
Top
├── Run (Generating / RunningTools / AwaitingApproval)  ← loop_core turn loop
└── Done   ← terminal; stores the result fact "out"
```

On `Entry`, `Run` emits a `task` turn request (`prompts::task_request`, with the
write/build tool subset) via `Effect::CallLlm { kind: "turn" }`. Tool calls come
back through the kernel as `ToolSucceeded` / `ToolFailed` events, each gated by
the child's permission policy. When the model produces a final tool-free turn,
`TaskMachine` writes its result to the `out` fact (the `RESULT_FACT`):

```json
{ "task": "...", "summary": "...", "ok": true, "payload": { ... } }
```

`ok` is `true` only when the decision `status` is `proceed`. On `LlmFailed` it
records `ok: false` and still transitions to `Done`. Either way the child reaches
a terminal state with a harvestable `out` fact.

---

## The production `SdlcChildSpawner`

`SdlcChildSpawner` (`bootstrap/src/child_spawner.rs`) is the production
`ChildSpawner` for SDLC fan-out. For each task it builds a **fully isolated child
kernel**:

- a fresh `Context`,
- a `TurnExecutor` with its own isolated `ConversationStore` and
  `call_id → thread` registry,
- a `CompositeExecutor` with `TurnExecutor` + `ToolExecutor` + timers,
- a one-shot `TaskMachine` (using `loop_core` state handlers) running under its
  own `Runtime`,
- a **tightened child policy** that allows all core capabilities (`ReadFile`,
  `WriteFile`, `ExecuteShell`, `GitOperation`, `NetworkAccess`, `Rollback`)
  globally, but installs **no `UserExecutor`** - so any approval-gated call
  results in `ToolFailed` rather than blocking on a human. The kernel still gates
  every tool call through the permission policy; children are simply headless
  (auto-deny on approval requests).
- its own cancel slot so siblings never clobber each other.

It spawns the child runtime and a **spawn-and-forget** harvester task that waits
for the child to finish, reads the `out` fact, and posts
`SubmachineCompleted { machine, result }` to the parent. Because the harvest runs
on its own task, the parent loop is never blocked and siblings run concurrently.

`RuntimeBuilder` installs an `SdlcChildSpawner` only in `sdlc` mode, and only
then seeds the parent's `parallel_execution` fact - so a runtime without a
spawner never emits orphaned `InstantiateSubmachine` effects.

---

## The Execution fan-out / aggregation flow

Fan-out lives entirely in the `Execution` state of the `SdlcMachine`
(`core/src/machines/sdlc/mod.rs`):

1. **Decide to fan out.** On `Entry`, Execution reads `plan_payload` and extracts
   its `tasks` array (`tasks_of`). `should_fan_out` is true when there are **≥2
   tasks** and the `parallel_execution` fact is set (i.e. a spawner is installed).
2. **Fan out.** It seeds `exec_remaining = tasks.len()` and `exec_results = []`,
   then emits one `Effect::InstantiateSubmachine { machine: new id, descriptor: {
   index, task } }` per task. The runtime hands each to the `SdlcChildSpawner`.
3. **Aggregate.** Each child completion arrives as
   `InternalEvent::SubmachineCompleted { result }`. Execution pushes `result` into
   `exec_results` and decrements `exec_remaining`. While `remaining > 0` it simply
   `Reaction::handled()` (waits for more).
4. **Synthesise.** When the last child reports, it merges the per-child summaries
   into a single digest (`merge_child_results`), stores it as `execution_summary`,
   and **appends one synthesis user turn** to the `execution` thread (append-only)
   asking the model to integrate the results, resolve conflicts, and confirm
   completion - i.e. it re-deliberates. From there the normal Execution decision
   routing applies (`proceed` → Verification, etc.).

If the plan is single-track (or no spawner is installed), Execution skips all of
this and runs a single execution deliberation instead.

---

## Known limitation: children have no user-gate channels

Fanned-out child task kernels are built by `SdlcChildSpawner` **without a
`UserExecutor`** (no question/approval channels). They are designed to run
autonomously to a terminal result. Consequently:

> A child `TaskMachine`'s deliberation that returns `need_user_input` or
> `need_approval` does **not** pause to involve the developer. The `TaskMachine`
> only treats `proceed` as success; any other status (including
> `need_user_input` / `need_approval`) results in `ok: false` and the child
> terminates. In other words, **a child's request for human input is effectively
> terminal**, not a pause.

This is a deliberate, documented trade-off for the current parallel-execution
implementation: human gates are honoured at the *parent* SDLC level (Intake
scope confirmation, Planning approval, Delivery sign-off), but individual
fanned-out tasks cannot themselves ask the developer mid-flight. A task that
genuinely needs a human decision should be surfaced through the parent's planning
or recovery phases rather than relied upon to pause inside a child.

---

## Source of truth in code

- `hsm/src/runtime.rs` - `ChildSpawner`, `spawn_children`, the child
  registry, `note_child_completion`, `spawn_with_children`.
- `hsm/src/event.rs` - `InternalEvent::SubmachineCompleted { machine, result }`.
- `hsm/src/submachine.rs` - the synchronous in-process `Submachine<P>` path.
- `hsm/tests/child_spawner.rs` - proves concurrent fan-out and result
  aggregation (peak concurrent children ≥ 2; summed child results).
- `core/src/machines/loop_core.rs` - shared `Generating` / `RunningTools` /
  `AwaitingApproval` state handlers used by `TaskMachine` and `SdlcMachine`.
- `core/src/machines/sdlc/task.rs` - `TaskMachine`.
- `core/src/machines/sdlc/mod.rs` - the Execution fan-out/aggregation logic
  (`tasks_of`, `should_fan_out`, `merge_child_results`).
- `bootstrap/src/child_spawner.rs` - `SdlcChildSpawner` (uses `TurnExecutor`
  + `ToolExecutor` with tightened child policy).
- `bootstrap/src/runtime_builder.rs` - installs the spawner and the
  `parallel_execution` fact in `sdlc` mode.
