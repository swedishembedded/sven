-------------------------- MODULE EffectDelivery --------------------------
\* SPDX-License-Identifier: Apache-2.0
\* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
\*
\* What happens to an effect a transition emitted and the policy refused.
\*
\* This models `crates/kernel/src/lib.rs::run_effects`, the other half of
\* `crates/hsm/src/effect.rs`'s design. A transition performs no I/O; it
\* RETURNS `Effect` values, and the kernel decides what may run. Splitting it
\* that way is what makes the machine pure and testable -- and it also means
\* the machine's progress now depends on somebody else delivering an answer.
\*
\* The kernel treats the two kinds of effect differently, and says so:
\* `CallTool` is classified per call, so a refused tool becomes an
\* `Event::ToolFailed` the machine sees as an ordinary tool result and can act
\* on. Everything else is validated all-or-nothing, "because non-tool effects
\* cannot fail gracefully mid-stream" -- the refusal is written to the audit
\* trail and published to observers, and nothing at all is sent back into the
\* machine.
\*
\* That asymmetry is what this model is about. A machine that transitions into
\* a state it can only leave on a reply, in a dispatch whose batch was
\* refused, is waiting for an event that nobody will ever post: not an error,
\* not a timeout, not a retry. The session is simply finished, with a healthy
\* looking state label.
\*
\* Two switches:
\*
\*   BatchAllOrNothing        -- one refused effect drops the whole non-tool
\*                               batch. TRUE is the kernel as written, and it
\*                               is a deliberate decision rather than an
\*                               oversight.
\*   RefusalFeedsBackAnEvent  -- a refused non-tool effect answers the machine
\*                               the way a refused tool already does. FALSE is
\*                               the kernel as written.
\*
\* Reachability today: the only non-tool effect that carries a capability is
\* `RollbackToCheckpoint`, and no machine in the workspace emits one yet, so
\* this is a property of the kernel's contract rather than a live incident.
\* It is modelled because the contract is what the next machine will rely on.
\*
\* Swedish Embedded AB implements solutions for agent runtimes that answer
\* every request they refuse. If your team needs expertise in effect-based
\* kernel design then you can procure our services by sending an email to
\* info@swedishembedded.com.

EXTENDS FiniteSets

CONSTANTS BatchAllOrNothing, RefusalFeedsBackAnEvent

\* One dispatch's effects, by how the gate treats them:
\*   await          -- an allowed non-tool effect whose result the machine is
\*                     waiting for (CallLlm, ScheduleTimeout, Verify)
\*   refusedNonTool -- a non-tool effect the policy refuses in this state
\*                     (RollbackToCheckpoint with no approval granted)
\*   refusedTool    -- a CallTool the policy refuses
Kinds == {"await", "refusedNonTool", "refusedTool"}

\* The batches worth checking: an awaited effect alone, a refused one alone,
\* and -- the case the design question is about -- both from one transition.
Batches == { {"await"}, {"refusedTool"}, {"refusedNonTool"},
             {"await", "refusedNonTool"}, {"refusedTool", "refusedNonTool"} }

VARIABLES
    pc,        \* "ready" -> "gate" -> "ran" -> "ready"
    batch,     \* what the transition emitted
    executed,  \* effects handed to an executor and not yet answered
    dropped,   \* effects the gate refused to run
    inbox,     \* an event is waiting for the machine
    waiting    \* the machine transitioned into a state only a reply leaves

vars == <<pc, batch, executed, dropped, inbox, waiting>>

Init ==
    /\ pc = "ready"
    /\ batch = {}
    /\ executed = {}
    /\ dropped = {}
    /\ inbox = FALSE
    /\ waiting = FALSE

\* A dispatch emits its effects. The machine is left waiting if any of them is
\* one whose answer is its only way on.
Emit(b) ==
    /\ pc = "ready"
    /\ pc' = "gate"
    /\ batch' = b
    /\ waiting' = (({"await", "refusedTool"} \cap b) # {})
    /\ executed' = {}
    /\ dropped' = {}
    /\ inbox' = FALSE

\* `run_effects`.
Gate ==
    /\ pc = "gate"
    /\ LET nonTool == batch \cap {"await", "refusedNonTool"}
           refused == "refusedNonTool" \in batch
           drop    == IF refused /\ BatchAllOrNothing THEN nonTool
                      ELSE IF refused THEN {"refusedNonTool"}
                      ELSE {}
       IN /\ executed' = nonTool \ drop
          /\ dropped' = drop
          \* A refused tool is answered with ToolFailed; a refused non-tool
          \* effect is answered only if the design says so.
          /\ inbox' = \/ "refusedTool" \in batch
                      \/ (RefusalFeedsBackAnEvent /\ drop # {})
    /\ pc' = "ran"
    /\ UNCHANGED <<batch, waiting>>

\* An executed effect's result comes back as an event.
Reply ==
    /\ pc = "ran"
    /\ "await" \in executed
    /\ executed' = executed \ {"await"}
    /\ inbox' = TRUE
    /\ UNCHANGED <<pc, batch, dropped, waiting>>

\* A dispatch whose effects nobody has to answer simply ends: the machine is
\* in a state it can leave on its own, and the runtime goes back to waiting
\* for whatever the outside world does next.
Settle ==
    /\ pc = "ran"
    /\ ~waiting
    /\ ~inbox
    /\ pc' = "ready"
    /\ UNCHANGED <<batch, executed, dropped, inbox, waiting>>

\* The machine takes the event and moves on.
Consume ==
    /\ pc = "ran"
    /\ inbox
    /\ inbox' = FALSE
    /\ waiting' = FALSE
    /\ pc' = "ready"
    /\ UNCHANGED <<batch, executed, dropped>>

Next == \/ \E b \in Batches : Emit(b)
        \/ Gate \/ Reply \/ Settle \/ Consume

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Properties

TypeOK ==
    /\ pc \in {"ready", "gate", "ran"}
    /\ batch \subseteq Kinds
    /\ executed \subseteq Kinds
    /\ dropped \subseteq Kinds
    /\ inbox \in BOOLEAN
    /\ waiting \in BOOLEAN

\* The machine is never left waiting for an answer that no longer exists: it
\* has an event in hand, or an effect still running that will produce one.
\*
\* TODAY'S KERNEL DOES NOT SATISFY THIS. `EffectDeliveryAsShipped.cfg` keeps
\* it failing rather than leaving it implied. Closing it is real work rather
\* than a missing `if`: the machine has to be TOLD, which means a refusal
\* event in `sven_vocab::SessionEvent` that every machine can act on, and a
\* decision about what a machine that ignores it should then do. A refusal
\* that only reaches the audit trail and the observation plane reaches nobody
\* who can do anything about it.
NoSilentStall ==
    ~(pc = "ran" /\ waiting /\ ~inbox /\ "await" \notin executed)

\* An effect the policy allows runs, even when another effect of the same
\* dispatch was refused.
\*
\* The kernel deliberately does not satisfy this for non-tool effects, and the
\* comment in `run_effects` says why: it treats the batch as one indivisible
\* intent, so a dispatch that wanted to roll back AND ask the model what to do
\* next does neither rather than half of it. The cost is what
\* `EffectDeliveryPerEffect.cfg` measures: per-effect gating -- what the tool
\* path already does -- keeps the innocent effect alive.
InnocentEffectSurvivesARefusal ==
    (pc = "ran") => ("await" \in batch => "await" \notin dropped)

============================================================================
