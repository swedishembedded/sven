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

That is the whole of it, and deliberately so. Every machine in this workspace
holds only a `MachineId` in its own fields - all durable state lives in
`Context` - so the active state plus the context is complete. A snapshot holds
no connections, channels or executors; those belong to whatever resumes it.

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
let mut machine = registry.get("agent").unwrap()();
machine.restore_state(&snap.state)?;
```

## Resuming versus replaying

Both reconstruct a machine; they answer different questions.

| | `Hsm::restore` | `replay` |
|---|---|---|
| Cost | O(1) | O(events) |
| Needs | A snapshot | The full event log |
| Rebuilds context | From the snapshot | By re-dispatching every event |
| Use for | Advancing a session one step | Auditing, and verifying determinism |

`replay` returns `(Hsm<M>, Context)`. The context is the point of replaying -
the audit trail, granted capabilities and accumulated facts - so it is returned
rather than discarded.

## The obligation on a machine

**A machine must implement `Machine::all_states()`.** It is how a snapshot's
state label is mapped back to a state value. A machine that leaves it at the
empty default cannot be resumed: `restore` fails with
`RestoreError::UnknownState`, naming the state it could not resolve and the
states it does know.

Failing is the point. Resuming a machine that cannot resolve its own state would
silently drop it back to its initial state and re-run work that has already
happened - a corruption that would surface much later, as a duplicated action
rather than as an error.

`crates/machines/tests/restorable.rs` asserts this for every mode in
`ModeRegistry::default_registry()`, so a new mode that omits `all_states()`
fails the suite rather than failing in production.

**A machine must keep durable state in `Context`, not in its own fields.** A
field is not captured by a snapshot and will silently revert on resume.
