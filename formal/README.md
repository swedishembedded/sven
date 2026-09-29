# Formal models

Machine-checked models of the parts of sven whose correctness argument is
otherwise only a comment and a handful of example transitions.

Run them with `make formal`. It is not part of `make check`: the models need a
JRE and a 2 MB download, and `check` is the inner loop.

## What this is for

`crates/hsm` is the whole of sven's control flow. Every agent loop, every
permission decision and every audit record is downstream of it, so the parts
of it that are true "for all states and all events" are exactly the parts a
test cannot reach. A test shows that the transition its author thought of
does the right thing. It cannot show that no reachable configuration and no
event produce a state the machine was never supposed to be in. That second
statement is what these models are for.

Three rules keep the suite honest.

**Every model names the code it models.** A model that has drifted from the
implementation is worse than no model, because it still passes. Each spec's
header says which file it is a model of and which claim in that file it is
checking, so drift is at least visible to anyone editing either side.

**Some models are supposed to fail.** Where the code made a design choice with
a plausible alternative, the spec is parameterised by that choice and run both
ways: once as written (must pass) and once as the rejected alternative (must
fail, on a named property). `formal/run-tla.sh` checks both outcomes. A suite
in which everything passes cannot tell you whether it is checking anything.

**A gap is checked in, not left implied.** Where a model finds a property the
code does not satisfy and closing it is real work, the failing configuration
stays in the suite as a `gap:` entry. It is reported as `ok (GAP)`, separately
from a rejected design, and the runner tells you to promote it the day it
starts passing. `Submachine.tla`'s `NoOrphanedChild` and
`EffectDelivery.tla`'s `NoSilentStall` are the two today.

A third kind is kept apart from both: a `tradeoff:` entry is a property the
code gives up ON PURPOSE, priced by a sibling configuration that shows what
keeping it would cost. It is reported as `ok (COST)`. A gap that starts
passing is progress; a trade-off that starts passing means somebody changed
the design and should say so.

## What is checked today

### `Hsm.tla` -- the engine's configuration is always the ancestor chain

Models `crates/hsm/src/dispatch.rs`: the two-phase engine every sven agent
loop runs on. Phase 1 walks the `Super` chain from the active leaf until some
state handles the event; phase 2 exits from the active leaf to the least
common ancestor of the handling state and the target, fires the transition
action, enters down to the target, and drills through composites' `Init`
transitions.

The property that holds all of it together is one line: between dispatches,
the set of states the machine is inside is exactly the active leaf and its
ancestors, each entered once and exited once. Nothing in the code computes
that set -- which is the point. It is what a caller believes when it reads a
state label, and the algorithm has to earn it on every path.

Checked: `TypeOK`, `ConfigurationIsTheAncestorChain`,
`EachStateEnteredAndExitedOnce`, `CommonAncestorUndisturbed` (a transition
between two states under a shared ancestor does not exit and re-enter it),
`SelfTransitionRestartsItsState`, `EveryEventResolves` (every state/event pair
ends in a transition, an internal transition, or an explicit refusal -- nothing
falls off the top of the hierarchy undefined), `NoDeadEnd`,
`RestsWhereItCanRun`, and `DispatchTerminates`.

Assumed, because `Machine`'s contract states it and the engine
`debug_assert!`s it: `superstate` reaches the root from every state, so the
phase-1 walk and the exit walk terminate. A machine that violates that hangs
the engine before any property here has anything to say. The `Init` drill is
deliberately NOT assumed well formed -- see below.

The model found two things.

**The `Init` drill had no bound.** `drill_into_composites` re-dispatches `Init`
to whatever state it just entered and follows the answer. Nothing in
`Machine`'s signature constrains an `Init` target to a descendant, so two
states whose `Init` name each other spin in that loop forever -- inside the
runtime's single consumer task, so the agent stops answering with no error, no
log line and no way back. The engine now stops at a state it would enter twice
in one drill (and `debug_assert!`s, because it is a contract violation).
`HsmUnguardedDrill.cfg` is the loop as it was; `HsmGuardedDrill.cfg` is the
same malformed machine with the guard. The guarded run deliberately checks
only termination: a machine whose `Init` names a non-descendant has already
broken the hierarchy contract and the guard does not promise to repair it,
only to turn a hung agent into a wrong state.

**A snapshot could be resumed into a composite.** `Machine::all_states()`
enumerates composites too, because coverage tooling needs them, and
`restore_in_place` accepted anything it enumerated -- so a session could be
resumed into `Session` rather than into `Idle` or `Generating` beneath it. A
running machine never rests there: every dispatch drills past a composite into
a substate. Resumed into one, the substate's entry action has not run and
every event the substates handle is ignored, so the agent sits in the
superstate answering nothing. `restore` now refuses with
`RestoreError::CompositeState`, and `ErasedMachine::all_state_labels` reports
only what can actually be resumed. `HsmRestoreAnyState.cfg` is the old
behaviour.

`HsmExitFromSource.cfg`, `HsmLcaShortcut.cfg` and
`HsmLocalSelfTransition.cfg` are the three rejected designs: exiting from the
handling state rather than the active leaf, taking the source's parent as the
LCA, and treating a self-transition as local. Each must fail on its own named
property.

Deliberately not modelled here: effect *payloads*. `Effect`s are values a
transition returns, so within the engine an effect is emitted exactly once per
dispatch, in exit/action/entry/init order, and a transition has no way to
observe its own effect's result -- results arrive later as `Event`s, through
the queue, which is the separation `sven-kernel` enforces by construction
rather than by argument. What happens to an effect after the engine returns it
is `EffectDelivery.tla`, below.

### `Submachine.tla` -- a child hands control back exactly once

Models `crates/hsm/src/submachine.rs::Submachine`, the kernel's composition
primitive: while a child is installed every event goes to the child first,
what the child does not handle bubbles to the parent, and a child that reaches
its terminal state is dropped and announced as
`InternalEvent::SubmachineCompleted`.

The module's own doc states the promise: "When the child reaches its terminal
state, the parent is notified and the child is dropped." The word doing the
work is WHEN. The host looks for completion in exactly one place -- after
routing an event -- so the promise holds only if every way a child can become
terminal passes through that place. One does not: `instantiate_child` runs the
child's `init` and then asks nothing, so a child whose initial transition
lands in its terminal state (a factory asked for a step whose work turns out
to be done) was installed as if it were live. The parent was never told, and
the next event -- if one ever came -- was dispatched into a machine that had
already ended. `instantiate_child` now checks.

Checked: `TypeOK`, `NotifiedAtMostOnce`, `TerminalChildIsNotLeftInstalled`,
`NoEventReachesAFinishedChild`, `CompletionReachesTheOwner`.

**`SubmachineParentLeaves.cfg` is a tracked GAP, not a rejected design.**
`Submachine` records no link between a child and the parent state that owns
it, so a parent that transitions away leaves the child installed and still
FIRST in line for every event -- a machine belonging to a state nobody is in
any more, eventually announcing its completion to a parent that moved on.
Closing it is a design decision about agent semantics (does leaving cancel the
child or park it?) and needs ownership declared somewhere the kernel can read
it, so it is checked in failing rather than papered over. It is not reachable
from sven's own machines today, because nothing in the workspace composes with
`Submachine` -- the production child path is `sven-kernel`'s `ChildSpawner` --
but `Submachine` is public API of `sven-hsm` and this is what it promises.

### `EffectDelivery.tla` -- an effect that is refused is still an answer owed

Models `crates/kernel/src/lib.rs::run_effects`, the other half of
`crates/hsm/src/effect.rs`'s design. A transition performs no I/O; it returns
`Effect` values and the kernel decides what may run. That is what keeps the
machine pure -- and it also means the machine's progress now depends on
somebody else delivering an answer.

The kernel treats the two kinds of effect differently, and says so. A
`CallTool` is classified per call, so a refused tool comes back as
`Event::ToolFailed`, which the machine sees as an ordinary tool result and can
act on. Everything else is validated all-or-nothing, "because non-tool effects
cannot fail gracefully mid-stream": the refusal goes to the audit trail and to
observers, and nothing at all goes back into the machine.

The model asks what a machine waiting on that batch does next. Nothing: it sat
in the state the transition moved it into, waiting for a reply nobody would
post -- no error, no timeout, no retry -- with a healthy looking state label.

Checked: `TypeOK`, `NoSilentStall`, `InnocentEffectSurvivesARefusal`.

`EffectDeliveryRefusalAnswered.cfg` and `EffectDeliveryPerEffect.cfg` are the
two designs that pass, and they cost different amounts: answering the refusal
closes the stall while keeping the indivisible batch; gating each effect
separately -- what the tool path already does -- also keeps the allowed
effects of a refused dispatch running.

**`EffectDeliveryAsShipped.cfg` is a tracked GAP.** Closing it is not a
missing `if`: the machine has to be told, which means a refusal event in
`sven_vocab::SessionEvent` that machines can act on, and a decision about what
a machine that ignores it should then do.

**`EffectDeliveryBatchDrop.cfg` is a priced TRADE-OFF, not a gap.** The
all-or-nothing batch is deliberate: a dispatch that wanted a gated side
effect AND to ask the model what to do next does neither rather than half of
it. The
configuration is checked in failing so the price stays visible and so that
changing the batch rule cannot happen silently.

Reachability today: no non-tool effect carries a capability, so the policy
refuses none and the stall is a property of the kernel's contract rather than
a live incident. It is modelled because the contract is what the first
capability-carrying non-tool effect will rely on.

## The tools

TLC comes from the pinned, SHA-256-verified `tla2tools.jar` that
`formal/fetch-tools.sh` downloads into the gitignored `formal/tools/`. The jar
is third-party and is never committed; the pin and the checksum are, so what
ran is what the suite says ran.

## Adding a model

1. Write `formal/tla/<Name>.tla` as a statement about a design, with no
   concrete scenario in it.
2. Where the claim needs a concrete instance -- a particular hierarchy, a
   particular set of callers -- put it in `formal/tla/MC_<Name>.tla`, which
   `INSTANCE`s the spec. That is what keeps the spec readable as a claim
   rather than as a test fixture. A spec with no scenario constants
   (`Submachine.tla`) does not need one.
3. Write `<Name>.cfg`, and where there is a rejected alternative worth pinning
   down, `<Name><Alternative>.cfg` too.
4. Add both to the `suite` table in `formal/run-tla.sh` with their expected
   outcomes.
5. Add a section here saying what it establishes AND what it assumes.

---

Swedish Embedded AB implements solutions for proving that a control-flow
kernel behaves the way its designers believe it does, before an agent acts on
a configuration nobody modelled. If your team needs expertise in formal
specification and model checking of real systems then you can procure our
services by sending an email to info@swedishembedded.com.
