------------------------------- MODULE Goal -------------------------------
\* SPDX-License-Identifier: Apache-2.0
\* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
\*
\* A standing goal: a person's commands, the kernel that runs the goal, the
\* executors that answer its turns and checks, and the surface that shows one
\* answer per input, all interleaved.
\*
\* This models `crates/machines/src/machines/reactive_agent.rs` with
\* `goal_flow.rs` (where a goal turn starts and where the person's turn ends)
\* and the surfaces in `crates/remote/src/host.rs` and the TUI (an answer is
\* open from an input at rest until `TurnComplete`). The policy that decides
\* after each turn (`goal.rs`) is abstracted to its outcome classes here and
\* proved in full in `formal/lean/Sven/Goal.lean`.
\*
\* The claims, for every interleaving of commands, replies, verdicts and
\* cancels the bounds allow:
\*
\*   NoGoalTurnUnlessActive -- a goal turn starts only while the goal is
\*       active: a pause or clear the kernel has processed is never followed
\*       by a turn of the goal it stopped.
\*   NoWorkAfterTheAnswerEnded -- a goal turn starts only while the
\*       person's answer is open, so nothing it does arrives after the
\*       surface closed the answer (a server drops such events). The one
\*       exception is a turn the person has already stopped: the cancel
\*       queued behind it cuts it before it does anything that matters.
\*   CompletionNeedsAPassingCheck -- a goal with a check completes only on a
\*       passing verdict.
\*   EveryAnswerEnds -- (liveness) an open answer is eventually ended.
\*
\* Two design switches, each run as shipped and as the alternative it
\* rejected:
\*
\*   MachineEndsTurn -- TRUE (shipped): the kernel announces the end of the
\*       person's turn when it comes to rest. FALSE: the turn executor ends
\*       the turn after every reply without tool calls.
\*   KernelContinues -- TRUE (shipped): the kernel decides and starts the
\*       goal's next turn inside the dispatch that settled the last one.
\*       FALSE: the kernel comes to rest after each turn and the surface,
\*       told the goal is still active, sends a continuation message.
\*   ReportDecides -- FALSE (shipped): with a check set, only its verdict
\*       completes a goal. TRUE: the model's report completes it anyway.
\*
\* And one about the person: StopAnytime. FALSE: Stop cuts a model turn or
\* check that has not produced its result yet. TRUE: Stop at any moment,
\* also in the instant between a command's answer being sent and arriving.
\* A turn's end carries no reference to the input it answers, so a stopped
\* answer's late end then closes the next answer the person opened - for a
\* plain message as much as for a goal. That is a gap of the session
\* protocol, tracked in `.agents/roadmap/goals.md`.
\*
\* Swedish Embedded AB implements solutions for agents that keep working
\* toward an objective without running away from the person who set it. If
\* your team needs expertise in supervised agent runtimes then you can
\* procure our services by sending an email to info@swedishembedded.com.

EXTENDS Naturals, Sequences

CONSTANTS MachineEndsTurn, KernelContinues, ReportDecides, StopAnytime,
          MaxTurns,   \* goal turns per goal (a small budget)
          MaxInputs   \* person inputs over a whole behaviour

Statuses == {"active", "paused", "blocked", "completed", "budget"}
Commands == {"set", "pause", "resume", "clear", "check"}

VARIABLES
    m,          \* the machine: "Idle", "Gen" (a turn in flight), "Check"
    has,        \* the session has a goal
    status,     \* its status
    turns,      \* goal turns taken
    check,      \* a check is set
    verified,   \* completed on a passing check
    llm,        \* a model turn in flight: "none" | "pending"
    chk,        \* a check in flight: "none" | "pending"
    inbox,      \* the kernel's event queue
    outbox,     \* turn ends on their way to the surface
    answer,     \* the surface has an answer open
    inputs,     \* inputs the person has made
    badTurn,    \* a goal turn started while the goal was not active
    lostWork,   \* a goal turn started with no answer open
    unearned    \* a goal with a check completed without a passing verdict

vars == <<m, has, status, turns, check, verified, llm, chk, inbox, outbox,
          answer, inputs, badTurn, lostWork, unearned>>

TypeOK ==
    /\ m \in {"Idle", "Gen", "Check"}
    /\ has \in BOOLEAN /\ status \in Statuses /\ turns \in 0..MaxTurns
    /\ check \in BOOLEAN /\ verified \in BOOLEAN
    /\ llm \in {"none", "pending"} /\ chk \in {"none", "pending"}
    /\ answer \in BOOLEAN /\ inputs \in 0..MaxInputs

Active == has /\ status = "active"
Resumable == has /\ status \in {"paused", "blocked", "budget"}

Init ==
    /\ m = "Idle" /\ has = FALSE /\ status = "paused" /\ turns = 0
    /\ check = FALSE /\ verified = FALSE
    /\ llm = "none" /\ chk = "none"
    /\ inbox = <<>> /\ outbox = <<>> /\ answer = FALSE /\ inputs = 0
    /\ badTurn = FALSE /\ lostWork = FALSE /\ unearned = FALSE

\* The kernel comes to rest; with MachineEndsTurn it ends the person's turn.
\* `cont` is what the surface is told when the kernel does not continue.
Rest(cont) ==
    /\ m' = "Idle"
    /\ outbox' = IF MachineEndsTurn \/ ~KernelContinues
                 THEN Append(outbox, [cont |-> cont]) ELSE outbox

\* The person has stopped the answer and the kernel has not yet cut the turn:
\* a turn that starts now is cut by the cancel queued behind it.
StopQueued == \E i \in 1..Len(inbox) : inbox[i].e = "cancel"

\* A goal turn starts, while the goal is `active` (or not): recorded against
\* the two safety claims. The kernel never changes `answer`.
StartGoalTurn(active) ==
    /\ m' = "Gen" /\ llm' = "pending"
    /\ badTurn' = (badTurn \/ ~active)
    /\ lostWork' = (lostWork \/ (~answer /\ ~StopQueued))

----------------------------------------------------------------------------
\* The person. A command sent at rest opens an answer and waits for it
\* (`awaits`); one sent while an answer is open changes what follows. Stop
\* closes the answer at once and cancels the turn.

Send(c) ==
    /\ inputs < MaxInputs
    /\ inputs' = inputs + 1
    /\ inbox' = Append(inbox, [e |-> c, awaits |-> ~answer])
    /\ answer' = TRUE
    /\ UNCHANGED <<m, has, status, turns, check, verified, llm, chk, outbox,
                   badTurn, lostWork, unearned>>

\* A reply or verdict already on its way to the kernel.
ResultQueued == \E i \in 1..Len(inbox) : inbox[i].e \in {"done", "working", "pass", "fail"}

\* Stop cuts the model turn or check in flight at once (the executor's cancel
\* handle), and the kernel hears of it after whatever is queued before.
Stop ==
    /\ inputs < MaxInputs /\ answer
    /\ \/ StopAnytime
       \/ (llm = "pending" \/ chk = "pending") /\ outbox = <<>> /\ ~ResultQueued
    /\ inputs' = inputs + 1
    /\ inbox' = Append(inbox, [e |-> "cancel", awaits |-> FALSE])
    /\ answer' = FALSE
    /\ llm' = "none" /\ chk' = "none"
    /\ UNCHANGED <<m, has, status, turns, check, verified, outbox,
                   badTurn, lostWork, unearned>>

----------------------------------------------------------------------------
\* The executors: a model turn ends with a reply (it reported the goal
\* complete, or not); a check ends with a verdict. Without MachineEndsTurn
\* the turn executor ends the person's turn with every reply.

Reply(r) ==
    /\ llm = "pending"
    /\ llm' = "none"
    /\ inbox' = Append(inbox, [e |-> r, awaits |-> FALSE])
    /\ outbox' = IF MachineEndsTurn THEN outbox
                 ELSE Append(outbox, [cont |-> FALSE])
    /\ UNCHANGED <<m, has, status, turns, check, verified, chk, answer,
                   inputs, badTurn, lostWork, unearned>>

Verdict(v) ==
    /\ chk = "pending"
    /\ chk' = "none"
    /\ inbox' = Append(inbox, [e |-> v, awaits |-> FALSE])
    /\ UNCHANGED <<m, has, status, turns, check, verified, llm, outbox,
                   answer, inputs, badTurn, lostWork, unearned>>

----------------------------------------------------------------------------
\* The surface ends the answer at a turn end; told the goal continues, it
\* sends the continuation itself (KernelContinues = FALSE only).

Surface ==
    /\ outbox # <<>>
    /\ outbox' = Tail(outbox)
    /\ IF Head(outbox).cont
       THEN /\ inbox' = Append(inbox, [e |-> "continue", awaits |-> FALSE])
            /\ answer' = TRUE
       ELSE /\ answer' = FALSE /\ UNCHANGED inbox
    /\ UNCHANGED <<m, has, status, turns, check, verified, llm, chk, inputs,
                   badTurn, lostWork, unearned>>

----------------------------------------------------------------------------
\* The kernel, one event at a time.

\* After a goal turn settled with `done` (the model reported it complete):
\* the check if there is one, else the report, else the budget, else on.
Settle(done) ==
    LET t == turns + 1 IN
    /\ turns' = t
    /\ IF check /\ ~(done /\ ReportDecides)
       THEN /\ m' = "Check" /\ chk' = "pending"
            /\ UNCHANGED <<status, verified, llm, outbox, badTurn, lostWork>>
       ELSE IF done
       THEN /\ status' = "completed" /\ verified' = FALSE
            /\ Rest(FALSE) /\ UNCHANGED <<llm, chk, badTurn, lostWork>>
       ELSE IF t >= MaxTurns
       THEN /\ status' = "budget" /\ Rest(FALSE)
            /\ UNCHANGED <<verified, llm, chk, badTurn, lostWork>>
       ELSE IF KernelContinues
       THEN /\ StartGoalTurn(Active) /\ UNCHANGED <<status, verified, chk, outbox>>
       ELSE /\ Rest(TRUE) /\ UNCHANGED <<status, verified, llm, chk, badTurn, lostWork>>

\* The answer to a command the person waits for: it ends their turn.
Answer(awaits) ==
    outbox' = IF awaits THEN Append(outbox, [cont |-> FALSE]) ELSE outbox

\* The commands. Only a command the person waits for, at rest, starts a
\* goal turn; every command the person waits for is answered once - by its
\* own end, or by the end of the work it started.
Dispatch ==
    /\ inbox # <<>>
    /\ LET e == Head(inbox).e
           a == Head(inbox).awaits IN
       /\ inbox' = Tail(inbox)
       /\ CASE e = "set" ->
                IF m = "Idle" /\ a
                THEN /\ has' = TRUE /\ status' = "active" /\ turns' = 0
                     /\ check' = FALSE /\ verified' = FALSE
                     /\ StartGoalTurn(TRUE)
                     /\ UNCHANGED <<chk, outbox, answer>>
                ELSE /\ Answer(a)
                     /\ UNCHANGED <<m, has, status, turns, check, verified, llm,
                                    chk, answer, badTurn, lostWork>>
            [] e = "pause" ->
                /\ status' = IF Active THEN "paused" ELSE status
                /\ Answer(a)
                /\ UNCHANGED <<m, has, turns, check, verified, llm, chk, answer,
                               badTurn, lostWork>>
            [] e = "clear" ->
                /\ has' = FALSE
                /\ Answer(a)
                /\ UNCHANGED <<m, status, turns, check, verified, llm, chk, answer,
                               badTurn, lostWork>>
            [] e = "check" ->
                /\ check' = (has \/ check)
                /\ Answer(a)
                /\ UNCHANGED <<m, has, status, turns, verified, llm, chk, answer,
                               badTurn, lostWork>>
            [] e = "resume" ->
                IF Resumable /\ m = "Idle" /\ a
                THEN /\ status' = "active" /\ turns' = 0
                     /\ StartGoalTurn(TRUE)
                     /\ UNCHANGED <<has, check, verified, chk, outbox, answer>>
                ELSE IF Resumable /\ m # "Idle"
                \* Goes on after the turn running, in its answer.
                THEN /\ status' = "active" /\ turns' = 0
                     /\ Answer(a)
                     /\ UNCHANGED <<m, has, check, verified, llm, chk, answer,
                                    badTurn, lostWork>>
                ELSE /\ Answer(a)
                     /\ UNCHANGED <<m, has, status, turns, check, verified, llm,
                                    chk, answer, badTurn, lostWork>>
            [] e = "cancel" ->
                \* The turn is cut (its abort ends the answer, closed already).
                /\ status' = IF Active THEN "paused" ELSE status
                /\ m' = "Idle" /\ llm' = "none" /\ chk' = "none"
                /\ UNCHANGED <<has, turns, check, verified, outbox, answer,
                               badTurn, lostWork>>
            [] e = "continue" ->
                \* A message: the kernel starts the turn it asks for.
                IF m = "Idle"
                THEN /\ StartGoalTurn(Active)
                     /\ UNCHANGED <<has, status, turns, check, verified, chk,
                                    outbox, answer>>
                ELSE UNCHANGED <<m, has, status, turns, check, verified, llm,
                                 chk, outbox, answer, badTurn, lostWork>>
            [] e \in {"done", "working"} ->
                IF m # "Gen"
                THEN UNCHANGED <<m, has, status, turns, check, verified, llm,
                                 chk, outbox, answer, badTurn, lostWork>>
                ELSE IF Active
                THEN /\ Settle(e = "done") /\ UNCHANGED <<has, check, answer>>
                ELSE /\ Rest(FALSE)
                     /\ UNCHANGED <<has, status, turns, check, verified, llm,
                                    chk, answer, badTurn, lostWork>>
            [] e \in {"pass", "fail"} ->
                IF m # "Check"
                THEN UNCHANGED <<m, has, status, turns, check, verified, llm,
                                 chk, outbox, answer, badTurn, lostWork>>
                ELSE IF e = "pass" /\ has
                THEN /\ status' = "completed" /\ verified' = TRUE
                     /\ Rest(FALSE)
                     /\ UNCHANGED <<has, turns, check, llm, chk, answer,
                                    badTurn, lostWork>>
                ELSE IF Active /\ turns < MaxTurns /\ KernelContinues
                THEN /\ StartGoalTurn(Active)
                     /\ UNCHANGED <<has, status, turns, check, verified, chk,
                                    outbox, answer>>
                ELSE IF Active /\ turns < MaxTurns
                THEN /\ Rest(TRUE)
                     /\ UNCHANGED <<has, status, turns, check, verified, llm,
                                    chk, answer, badTurn, lostWork>>
                ELSE /\ status' = IF Active THEN "budget" ELSE status
                     /\ Rest(FALSE)
                     /\ UNCHANGED <<has, turns, check, verified, llm, chk,
                                    answer, badTurn, lostWork>>
       /\ UNCHANGED inputs

\* The completion record: a goal with a check that becomes completed without
\* the verdict in hand would set this.
Unearned ==
    unearned' = (unearned \/ (has' /\ check' /\ status' = "completed"
                              /\ status # "completed" /\ ~verified'))

Next ==
    \/ /\ \/ \E c \in Commands : Send(c)
          \/ Stop
          \/ \E r \in {"done", "working"} : Reply(r)
          \/ \E v \in {"pass", "fail"} : Verdict(v)
          \/ Surface
       /\ UNCHANGED unearned
    \/ Dispatch /\ Unearned

Fairness ==
    /\ WF_vars(Dispatch /\ Unearned)
    /\ WF_vars(Surface /\ UNCHANGED unearned)
    /\ \A r \in {"done", "working"} : WF_vars(Reply(r) /\ UNCHANGED unearned)
    /\ \A v \in {"pass", "fail"} : WF_vars(Verdict(v) /\ UNCHANGED unearned)

Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
NoGoalTurnUnlessActive == ~badTurn
NoWorkAfterTheAnswerEnded == ~lostWork
CompletionNeedsAPassingCheck == ~unearned
TurnsWithinBudget == turns <= MaxTurns

\* An answer the person opened is eventually ended, by the kernel at rest or
\* by the person's stop.
EveryAnswerEnds == answer ~> ~answer
==============================================================================
