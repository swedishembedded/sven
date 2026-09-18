# ADR 0003: Sven is a framework; agents are typed, resumable objects

## Status

Accepted and implemented.

`sven-sdk` (directory `crates/sdk`) publishes `Engine`, `Agent`, `AgentState`,
`Method<T>` and the `#[agent]` attribute; the kernel is suspendable via
`Hsm::snapshot`/`Hsm::restore` and `ErasedRuntime::capture`; `replay` returns
the context it reconstructs; every registered machine enumerates its states.
Applications extend the framework through `EngineBuilder::tool` and
`EngineBuilder::machine` without editing any crate here, and `sven agent step`
is the in-repo consumer that keeps the surface honest.

Described in [docs/technical/sdk.md](../technical/sdk.md) and
[docs/technical/resumable-agents.md](../technical/resumable-agents.md).

Not done: migrating the existing surfaces (`sven-tui`, `sven-ci`, `sven-acp`,
`sven-mcp`) onto the SDK. They construct kernels through `RuntimeBuilder`
directly and need capabilities the SDK does not yet publish - interactive
approval interception, mid-session mode and model changes, and token-level
streaming control. Growing the SDK to cover them is worth doing only if it can
be done without turning the facade back into `RuntimeBuilder` with different
names.

## Context

Sven began as an application: a binary with a kernel inside it. The root `sven`
crate is bin-only, so nothing outside the workspace can build an agent on top of
it, and the closest thing to a public entry point - `sven-bootstrap`'s
`RuntimeBuilder` - is an internal composition root with roughly twenty `with_*`
methods. An embedder has to understand `Config`, `ModelConfig`, `ToolRegistry`,
`EffectExecutor`, `EventSink`, `Principal` and `RuntimeHandle` before getting a
single turn out of it.

The intended shape is the inverse: a framework with a small, powerful public
interface, of which the `sven` CLI is merely the first consumer. Two properties
drive the design.

**An agent should be an ordinary software object.** A caller should write
`reviewer.assess(change)` and receive a validated `Assessment` or an explicit
failure. The sequence of model calls, tool calls and corrections belongs behind
that method boundary, so that dependency injection, unit testing, interface
substitution, composition and tracing all apply as they would to any other Rust
type. Today the kernel is reachable only as *a session that emits events*, never
as *a typed method that returns a value*.

**Agents must survive being put down.** The target deployment is a multi-tenant
service: a request arrives, the service loads that agent's state, executes one
step, persists, frees the resources, and waits. This forbids an agent that owns
live connections, and it forbids reconstructing state by replaying a whole event
log on every request.

## Decision

Structure sven as a framework in three layers, with the public surface in a new
`sven-sdk` crate (directory `crates/sdk`):

| Layer | Owns | Lifetime |
|-------|------|----------|
| `Engine` | Model provider clients, MCP manager, tool registry, model catalog, permission policy | Process |
| Agent instance | History, context blocks, working state, its call serialization lock | Task or role |
| Method contract | Role text, task text, result schema, execution strategy, budgets | Static |

The split is what makes the expensive resources shared while agent state stays
cheap enough to load, step, persist and drop. It is also what makes a
concurrency policy expressible: model-driven calls serialize *per agent
instance* so histories cannot interleave, while different instances run in
parallel over shared resources.

A consequence worth stating separately: **the stable prompt-cache prefix belongs
to the agent's class, not to its instance.** Many instances of one role share a
role prompt and a tool-schema block, so they should share a cache prefix and
diverge only in the per-instance suffix. `CompletionRequest`'s
`system_dynamic_suffix` and `core_tool_count` breakpoints already express this;
they are currently driven per session and would be driven per class.

### Principles adopted

These were evaluated against the existing kernel rather than adopted wholesale.
Most of the design vocabulary sven was already built on; the list below is only
what required new work.

- **The agent is a typed object, and an agentic loop is an ordinary method
  call.** The genuine gap. Everything else here follows from it.
- **Execution strategy is separate from the public contract.** The same method
  signature can be served by structured prediction (no tools) or by an iterative
  tool loop. Strategy is configuration, not a caller-facing argument.
- **Capability descriptions are derived from the interface**, so prompts and
  schemas cannot drift from the implementation they describe.
- **Snapshots are an explicit application workflow**, distinct from having a
  storage backend: what is captured, what must be reconstructed, and what must
  never be serialized.
- **Structure, invariants and real-world effects are validated separately**, and
  infrastructure faults keep their identity instead of being reported as model
  mistakes.

### Principles already satisfied, adopted as nothing

Recorded so they are not "implemented" a second time. Deterministic execution
versus model judgment, and mandatory workflow steps expressed in code, are the
kernel's founding rule. Explicit state lifetimes, cache-aware context layout,
separated validation, bounded and informative correction loops, subagent
isolation, enforcement-versus-observation, and whole-program tracing all have
existing homes - `loop_core`'s round budget, `verified_task`'s propose/verify/
retry cycle, `EffectExecutor` versus `EventSink`, ATIF trajectories, and
`sven-memory`'s provenance-aware recall, which already carries a safety property
(untrusted-provenance demotion) the source material does not.

### Principles rejected

**Code as a composable action language, live-object arguments, and sandboxed
containment.** These are one package; none works alone. Sven's thesis is that
every action is a typed `Effect`, permission-gated per capability. An agent that
acts by emitting code which calls methods directly routes *around* that gate.
Restoring the guarantee requires brokering every host operation from inside a
worker process - which is a re-implementation of `Effect` plus `ToolCapability`,
behind a process boundary, plus restricted serialization and resource limits.
The benefit, fewer model round trips for multi-step work, is partly available
already through multiple tool calls per round. Revisit only with a concrete
workload that the tool loop demonstrably cannot serve.

## Consequences

Every machine must enumerate its states via `Machine::all_states()`. This was
previously optional, used only by coverage tooling, and three of five machines
skipped it. It is now the mechanism by which a snapshot's state label is mapped
back to a state value, so a machine that omits it cannot be resumed at all. The
requirement is pinned by a test over every mode in the registry rather than left
to review.

Machines must keep durable state in `Context`, never in their own fields. Every
existing machine already holds only a `MachineId`, so this codifies current
practice rather than changing it - but a machine that broke the rule would
snapshot incorrectly and silently.

`replay` returns `(Hsm<M>, Context)` rather than `Hsm<M>`. Discarding the
reconstructed context made the audit trail, granted capabilities and domain
facts unrecoverable, which forced callers to rebuild a second machine by hand to
get at them.
