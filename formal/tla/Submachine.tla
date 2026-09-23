---------------------------- MODULE Submachine ----------------------------
\* SPDX-License-Identifier: Apache-2.0
\* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
\*
\* What happens to a child machine that finishes, and to one whose parent
\* moves on.
\*
\* This models `crates/hsm/src/submachine.rs::Submachine::dispatch`, the
\* kernel's composition primitive: while a child is installed every event goes
\* to the child first, whatever the child does not handle bubbles to the
\* parent, and a child that has reached its terminal state is dropped and
\* announced to the parent as `InternalEvent::SubmachineCompleted`.
\*
\* The module's own doc states the promise this checks: "When the child
\* reaches its terminal state, the parent is notified ... and the child is
\* dropped." The word doing the work is WHEN. The host looks for completion in
\* exactly one place -- after routing an event -- so whether the promise holds
\* depends on whether every way a child can become terminal passes through
\* that place. One does not: a child can be terminal the moment it is
\* installed, because `instantiate_child` runs its `init` and then asks
\* nothing.
\*
\* Two switches:
\*
\*   CompleteOnInstall -- `instantiate_child` checks for a child that its own
\*                        initial transition already finished. FALSE is the
\*                        code as it was.
\*   ParentCanLeave    -- the parent takes a transition of its own while a
\*                        child is live. This is not a design switch; it is
\*                        something a parent machine is free to do, and the
\*                        host has no way to notice.
\*
\* ChildStartsDone is a scenario, not a fault: a child whose work was already
\* complete when it was built -- a retry of a task that succeeded, a phase
\* whose precondition already holds -- is an ordinary thing for a factory to
\* return.
\*
\* Swedish Embedded AB implements solutions for composing agent state machines
\* that hand control back exactly once. If your team needs expertise in
\* hierarchical state-machine composition then you can procure our services by
\* sending an email to info@swedishembedded.com.

EXTENDS Integers

CONSTANTS CompleteOnInstall, ParentCanLeave, ChildStartsDone

VARIABLES
    child,      \* "absent", or the child's state: "Work" / "Done"
    parent,     \* "Owner" (the state that owns the child), "Elsewhere", "Finished"
    notified,   \* how many times SubmachineCompleted reached the parent
    flags       \* history: things that happened that were not supposed to

vars == <<child, parent, notified, flags>>

\* The events the host routes. "finish" is the one the child handles; the
\* others it ignores, so they bubble.
Evts == {"finish", "leave", "other"}

\* The parent's own reaction to a bubbled event. A parent that leaves the
\* owning state while a child is live is the case the host cannot see.
ParentStep(p, e) ==
    IF p = "Owner" /\ e = "leave" /\ ParentCanLeave THEN "Elsewhere" ELSE p

\* Delivering SubmachineCompleted.
ParentOnCompleted(p) == IF p = "Owner" THEN "Finished" ELSE p

NoFlags == [ toFinishedChild |-> FALSE, orphaned |-> FALSE ]

Init ==
    /\ child = "absent"
    /\ parent = "Owner"
    /\ notified = 0
    /\ flags = NoFlags

\* `instantiate_child`: build the child, run its `init`, install it.
Instantiate ==
    /\ child = "absent"
    /\ parent = "Owner"
    /\ notified = 0
    /\ LET born == IF ChildStartsDone THEN "Done" ELSE "Work" IN
       IF born = "Done" /\ CompleteOnInstall
       THEN /\ child' = "absent"
            /\ notified' = notified + 1
            /\ parent' = ParentOnCompleted(parent)
            /\ UNCHANGED flags
       ELSE /\ child' = born
            /\ UNCHANGED <<parent, notified, flags>>

\* `Submachine::dispatch` of one event.
Dispatch(e) ==
    IF child = "absent"
    THEN /\ parent' = ParentStep(parent, e)
         /\ UNCHANGED <<child, notified, flags>>
    ELSE LET wasDone  == child = "Done"
             handled  == e = "finish" /\ child = "Work"
             after    == IF e = "finish" THEN "Done" ELSE child
             bubbled  == IF handled THEN parent ELSE ParentStep(parent, e)
         IN IF after = "Done"
            THEN /\ child' = "absent"
                 /\ notified' = notified + 1
                 /\ parent' = ParentOnCompleted(bubbled)
                 /\ flags' = [ flags EXCEPT !.toFinishedChild = @ \/ wasDone ]
            ELSE /\ child' = after
                 /\ parent' = bubbled
                 /\ notified' = notified
                 /\ flags' = [ flags EXCEPT !.orphaned = @ \/ (bubbled # "Owner") ]

Next == Instantiate \/ \E e \in Evts : Dispatch(e)

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Properties

TypeOK ==
    /\ child \in {"absent", "Work", "Done"}
    /\ parent \in {"Owner", "Elsewhere", "Finished"}
    /\ notified \in 0..1
    /\ flags \in [ toFinishedChild: BOOLEAN, orphaned: BOOLEAN ]

\* A child's terminal state propagates to the parent exactly once. The "at
\* most" half: the host must not announce one completion twice.
NotifiedAtMostOnce == notified <= 1

\* And the "at least" half, as a safety property rather than a liveness one,
\* because the host has no later opportunity to notice: a terminal child must
\* never be left installed. One that is has not been announced, and will not be
\* until some unrelated event happens to arrive -- or never, if none does.
TerminalChildIsNotLeftInstalled == child # "Done"

\* A machine that has reached its terminal state is finished. Routing another
\* event into it dispatches on a state its author wrote as the end.
NoEventReachesAFinishedChild == ~flags.toFinishedChild

\* A parent transition while a submachine is live does not orphan it.
\*
\* TODAY'S CODE DOES NOT SATISFY THIS, and `SubmachineParentLeaves.cfg` keeps
\* it failing rather than leaving it implied. `Submachine` records no link
\* between the child and the parent state that owns it, so when the parent
\* transitions away the child stays installed and stays FIRST in line for
\* every event -- a machine belonging to a state nobody is in any more, still
\* consuming the events the parent's new state should be seeing, and
\* eventually announcing its completion to a parent that moved on.
\*
\* Closing it is real work rather than a check: the host would have to know
\* which parent state owns the child, which means the ownership has to be
\* declared somewhere the kernel can read it, and it has to decide what
\* leaving does -- cancel the child, or park it. That is a design decision
\* about agent semantics, not a missing `if`.
NoOrphanedChild == ~flags.orphaned

\* A completion that arrives after the parent has left is a completion nobody
\* handled. Same cause as the orphan; stated separately because this is the
\* half a caller sees.
CompletionReachesTheOwner == (notified = 1) => parent = "Finished"

============================================================================
