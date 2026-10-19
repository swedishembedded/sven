# Resumable agents

An agent session can be suspended to durable storage and resumed later without
replaying its event log. This is what lets a service handle one agent step per
request: load the state, advance it once, persist, free the resources, wait.

Design rationale, and the framework model this serves, are in
[ADR 0003](../adr/0003-agents-as-typed-objects.md).

## What a snapshot is

```rust
pub struct Snapshot {
    pub state: String,     // the active leaf state's `Debug` label
    pub context: Context,  // everything the machine accumulated
}
```

That is the whole of it, and deliberately so. Every machine the `ModeRegistry`
builds holds only a `MachineId` in its own fields - all durable state lives in
`Context` - so the active state plus the context is complete. A snapshot holds
no connections, channels or executors; those belong to whatever resumes it.

The `Context` carries the facts, tool results, retry counters, the pending
approval and pending question, the granted capabilities, the loaded tools and
active instructions, and the audit trails.

## Suspending and resuming

```rust
let snap = hsm.snapshot(&ctx);
let blob = serde_json::to_string(&snap)?;

// …later, in another process…
let snap: Snapshot = serde_json::from_str(&blob)?;
let (hsm, ctx) = Hsm::restore(ReactiveAgentMachine::new(), &snap)?;
```

A resumed machine is already initialized, so a subsequent `init()` is inert: the
initial transition does not re-fire, no entry action runs twice, and an
in-flight turn is not dropped back to `Idle`.

Where the concrete machine type is not nameable - a `Box<dyn ErasedMachine>` out
of the `ModeRegistry`, which is the usual case - use the in-place form:

```rust
let registration = registry.get("agent").unwrap();
let mut machine = (registration.factory)();
machine.restore_state(&snap.state)?;
```

(`Hsm::restore_in_place` is the same operation on an `Hsm`.)

## Suspending and resuming a live session

`ErasedRuntime::capture()` returns a `Snapshot` of a running session without
stopping it. It is served only once the event queue has drained, so the snapshot
shows the machine at rest rather than part-way through a turn. To resume,
`RuntimeBuilder::with_kernel_snapshot(snapshot)` starts the new session in the
snapshotted state with the snapshotted context instead of the machine's initial
state; building fails if the mode's machine does not enumerate that state. The
SDK's agents use this pair to carry a conversation across calls and processes
([SDK reference](../reference/sdk.md)).

A session built with `RuntimeBuilder::with_parked_approvals` (under manual
approval) ends with its own approval pending in `Context::pending_approval`
instead of asking on a channel. A later session built from that snapshot decides
it by posting `HumanApproved` or `HumanRejected`. A parked question
(`pending_question`) is resumed the same way, by `HumanAnswered`.

## Resuming versus replaying

Both reconstruct a machine; they answer different questions.

| | `Hsm::restore` | `replay` |
|---|---|---|
| Cost | O(1) | O(events) |
| Needs | A snapshot | The full event log |
| Rebuilds context | From the snapshot | By re-dispatching every event |
| Use for | Advancing a session one step | Auditing, and verifying determinism |

`sven_hsm::replay(factory, &events)` returns `(Hsm<M>, Context)`. The context is the point of replaying -
the audit trail, granted capabilities and accumulated facts - so it is returned
rather than discarded.

## The obligation on a machine

**A machine must implement `Machine::all_states()`.** It is how a snapshot's
state label is mapped back to a state value. A machine that leaves it at the
empty default cannot be resumed: `restore` fails with
`RestoreError::UnknownState`, naming the state it could not resolve and the
states it does know. A snapshot that names a composite state (one other states
live under, such as `Session` in the reactive machine) is refused with
`RestoreError::CompositeState`: a running machine always rests in a leaf.

Failing is the point. Resuming a machine that cannot resolve its own state would
silently drop it back to its initial state and re-run work that has already
happened - a corruption that would surface much later, as a duplicated action
rather than as an error.

`crates/machines/tests/restorable.rs` asserts this for every mode in
`ModeRegistry::default_registry()` and for `ui-test`, so a new mode that omits
`all_states()` fails the suite rather than failing in production.

**A machine must keep durable state in `Context`, not in its own fields.** A
field is not captured by a snapshot and will silently revert on resume.

Session state the kernel itself owns is in `Context` for the same reason. The
tools the model has loaded under deferred tool disclosure are
`Context::active_tools`: an ordered, append-only list that a snapshot restores
and a replay of the recorded `Event::ToolsActivated` events rebuilds, so a
resumed agent is offered the same tool definitions in the same order as the
one that was suspended (see [tool-disclosure.md](tool-disclosure.md)). The
skills, subagents and knowledge documents whose activation condition has held
are `Context::active_instructions`, restored and rebuilt the same way from
`Event::InstructionsActivated` (see
[instruction-disclosure.md](instruction-disclosure.md)).
