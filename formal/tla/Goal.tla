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
\* open from an input at rest until its turn ends, `TurnComplete` or
\* `Aborted`). The policy that decides
\* after each turn (`goal.rs`) is abstracted to its outcome classes here and
\* proved in full in `formal/lean/Sven/Goal.lean`.
\*
\* The claims, for every interleaving of commands, replies, verdicts and
\* cancels the bounds allow:
\*
\*   NoGoalTurnUnlessActive -- a goal turn starts only while the goal is
\*       active: a pause or clear the kernel has processed is never followed
\*       by a turn of the goal it stopped.
\*   NoWorkAfterTheAnswerEnded -- a goal turn starts only while the answer
\*       it belongs to is open, so nothing it does arrives after the surface
\*       closed that answer (a server drops such events). The one exception
\*       is a turn the person has already stopped: the cancel queued behind
\*       it cuts it before it does anything that matters.
\*   CompletionNeedsAPassingCheck -- a goal with a check completes only on a
\*       passing verdict.
\*   EveryAnswerEnds -- (liveness) an open answer is eventually ended.
\*
\* Four design switches, each run as shipped and as the alternative it
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
\*   AnswerIds -- TRUE (shipped): every input the surface shows as an answer
\*       is named (`answer_id`), the host names the answer of every event it
\*       sends (`host/answers.rs`), and the surface ends only the answer an
\*       end names, dropping the end of one it closed already. FALSE: an end
\*       closes whatever answer is open.
\*
\* And one about the person: StopAnytime. FALSE: Stop cuts a model turn or
\* check that has not produced its result yet. TRUE: Stop at any moment,
\* also in the instant between a command's answer being sent and arriving.
\* A stopped answer's end can then still be on its way when the person
\* opens the next answer; without AnswerIds it closes that one - for a plain
\* message as much as for a goal.
\*
\* Swedish Embedded AB implements solutions for agents that keep working
\* toward an objective without running away from the person who set it. If
\* your team needs expertise in supervised agent runtimes then you can
\* procure our services by sending an email to info@swedishembedded.com.

EXTENDS Naturals, Sequences

CONSTANTS MachineEndsTurn, KernelContinues, ReportDecides, AnswerIds, StopAnytime,
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
    outbox,     \* turn ends on their way to the surface, each naming its answer
    open,       \* the answer the surface has open (0: none); an input's id
                \* is its number among the person's inputs
    cur,        \* the answer the kernel's work belongs to
    inputs,     \* inputs the person has made
    badTurn,    \* a goal turn started while the goal was not active
    lostWork,   \* a goal turn started while its answer was not open
    unearned    \* a goal with a check completed without a passing verdict

vars == <<m, has, status, turns, check, verified, llm, chk, inbox, outbox,
          open, cur, inputs, badTurn, lostWork, unearned>>

answer == open # 0

TypeOK ==
    /\ m \in {"Idle", "Gen", "Check"}
    /\ has \in BOOLEAN /\ status \in Statuses /\ turns \in 0..MaxTurns
    /\ check \in BOOLEAN /\ verified \in BOOLEAN
    /\ llm \in {"none", "pending"} /\ chk \in {"none", "pending"}
    /\ open \in 0..MaxInputs /\ cur \in 0..MaxInputs /\ inputs \in 0..MaxInputs

Active == has /\ status = "active"
Resumable == has /\ status \in {"paused", "blocked", "budget"}

Init ==
    /\ m = "Idle" /\ has = FALSE /\ status = "paused" /\ turns = 0
    /\ check = FALSE /\ verified = FALSE
    /\ llm = "none" /\ chk = "none"
    /\ inbox = <<>> /\ outbox = <<>> /\ open = 0 /\ cur = 0 /\ inputs = 0
    /\ badTurn = FALSE /\ lostWork = FALSE /\ unearned = FALSE

\* The kernel comes to rest; with MachineEndsTurn it ends the person's turn.
\* `cont` is what the surface is told when the kernel does not continue.
Rest(cont) ==
    /\ m' = "Idle"
    /\ outbox' = IF MachineEndsTurn \/ ~KernelContinues
                 THEN Append(outbox, [cont |-> cont, id |-> cur]) ELSE outbox

\* The person has stopped the answer and the kernel has not yet cut the turn:
\* a turn that starts now is cut by the cancel queued behind it.
StopQueued == \E i \in 1..Len(inbox) : inbox[i].e = "cancel"

\* A goal turn of answer `id` starts, while the goal is `active` (or not):
\* recorded against the two safety claims. The kernel never changes `open`.
StartGoalTurn(active, id) ==
    /\ m' = "Gen" /\ llm' = "pending" /\ cur' = id
    /\ badTurn' = (badTurn \/ ~active)
    /\ lostWork' = (lostWork \/ (open # id /\ ~StopQueued))

----------------------------------------------------------------------------
\* The person. A command sent at rest opens an answer and waits for it
\* (`awaits`); one sent while an answer is open changes what follows. Stop
\* closes the answer at once and cancels the turn.

Send(c) ==
    /\ inputs < MaxInputs
    /\ inputs' = inputs + 1
    /\ inbox' = Append(inbox, [e |-> c, awaits |-> ~answer, id |-> inputs + 1])
    /\ open' = IF answer THEN open ELSE inputs + 1
    /\ UNCHANGED <<m, has, status, turns, check, verified, llm, chk, outbox,
                   cur, badTurn, lostWork, unearned>>

\* A reply or verdict already on its way to the kernel.
ResultQueued == \E i \in 1..Len(inbox) : inbox[i].e \in {"done", "working", "pass", "fail"}

\* Stop cuts the model turn or check in flight at once (the executor's cancel
\* handle), and the kernel hears of it after whatever is queued before.
Stop ==
    /\ inputs < MaxInputs /\ answer
    /\ \/ StopAnytime
       \/ (llm = "pending" \/ chk = "pending") /\ outbox = <<>> /\ ~ResultQueued
    /\ inputs' = inputs + 1
    /\ inbox' = Append(inbox, [e |-> "cancel", awaits |-> FALSE, id |-> open])
    /\ open' = 0
    /\ llm' = "none" /\ chk' = "none"
    /\ UNCHANGED <<m, has, status, turns, check, verified, outbox,
                   cur, badTurn, lostWork, unearned>>

----------------------------------------------------------------------------
\* The executors: a model turn ends with a reply (it reported the goal
\* complete, or not); a check ends with a verdict. Without MachineEndsTurn
\* the turn executor ends the person's turn with every reply.

Reply(r) ==
    /\ llm = "pending"
    /\ llm' = "none"
    /\ inbox' = Append(inbox, [e |-> r, awaits |-> FALSE, id |-> cur])
    /\ outbox' = IF MachineEndsTurn THEN outbox
                 ELSE Append(outbox, [cont |-> FALSE, id |-> cur])
    /\ UNCHANGED <<m, has, status, turns, check, verified, chk, open, cur,
                   inputs, badTurn, lostWork, unearned>>

Verdict(v) ==
    /\ chk = "pending"
    /\ chk' = "none"
    /\ inbox' = Append(inbox, [e |-> v, awaits |-> FALSE, id |-> cur])
    /\ UNCHANGED <<m, has, status, turns, check, verified, llm, outbox,
                   open, cur, inputs, badTurn, lostWork, unearned>>

----------------------------------------------------------------------------
\* The surface ends the answer at a turn end; told the goal continues, it
\* sends the continuation itself (KernelContinues = FALSE only). With
\* AnswerIds the end of an answer it closed already (stopped) is dropped.

Surface ==
    /\ outbox # <<>>
    /\ outbox' = Tail(outbox)
    /\ LET u == Head(outbox) IN
       IF AnswerIds /\ u.id # open
       THEN UNCHANGED <<inbox, open>>
       ELSE IF u.cont
       THEN /\ inbox' = Append(inbox, [e |-> "continue", awaits |-> FALSE, id |-> u.id])
            /\ open' = u.id
       ELSE /\ open' = 0 /\ UNCHANGED inbox
    /\ UNCHANGED <<m, has, status, turns, check, verified, llm, chk, cur, inputs,
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
            /\ UNCHANGED <<status, verified, llm, outbox, cur, badTurn, lostWork>>
       ELSE IF done
       THEN /\ status' = "completed" /\ verified' = FALSE
            /\ Rest(FALSE) /\ UNCHANGED <<llm, chk, cur, badTurn, lostWork>>
       ELSE IF t >= MaxTurns
       THEN /\ status' = "budget" /\ Rest(FALSE)
            /\ UNCHANGED <<verified, llm, chk, cur, badTurn, lostWork>>
       ELSE IF KernelContinues
       THEN /\ StartGoalTurn(Active, cur) /\ UNCHANGED <<status, verified, chk, outbox>>
       ELSE /\ Rest(TRUE) /\ UNCHANGED <<status, verified, llm, chk, cur, badTurn, lostWork>>

\* The answer to a command the person waits for, answer `id`: it ends their
\* turn.
Answer(awaits, id) ==
    outbox' = IF awaits THEN Append(outbox, [cont |-> FALSE, id |-> id]) ELSE outbox

\* The commands. Only a command the person waits for, at rest, starts a
\* goal turn; every command the person waits for is answered once - by its
\* own end, or by the end of the work it started.
Dispatch ==
    /\ inbox # <<>>
    /\ LET e == Head(inbox).e
           a == Head(inbox).awaits
           i == Head(inbox).id IN
       /\ inbox' = Tail(inbox)
       /\ CASE e = "set" ->
                IF m = "Idle" /\ a
                THEN /\ has' = TRUE /\ status' = "active" /\ turns' = 0
                     /\ check' = FALSE /\ verified' = FALSE
                     /\ StartGoalTurn(TRUE, i)
                     /\ UNCHANGED <<chk, outbox, open>>
                ELSE /\ Answer(a, i)
                     /\ UNCHANGED <<m, has, status, turns, check, verified, llm,
                                    chk, open, cur, badTurn, lostWork>>
            [] e = "pause" ->
                /\ status' = IF Active THEN "paused" ELSE status
                /\ Answer(a, i)
                /\ UNCHANGED <<m, has, turns, check, verified, llm, chk, open, cur,
                               badTurn, lostWork>>
            [] e = "clear" ->
                /\ has' = FALSE
                /\ Answer(a, i)
                /\ UNCHANGED <<m, status, turns, check, verified, llm, chk, open, cur,
                               badTurn, lostWork>>
            [] e = "check" ->
                /\ check' = (has \/ check)
                /\ Answer(a, i)
                /\ UNCHANGED <<m, has, status, turns, verified, llm, chk, open, cur,
                               badTurn, lostWork>>
            [] e = "resume" ->
                IF Resumable /\ m = "Idle" /\ a
                THEN /\ status' = "active" /\ turns' = 0
                     /\ StartGoalTurn(TRUE, i)
                     /\ UNCHANGED <<has, check, verified, chk, outbox, open>>
                ELSE IF Resumable /\ m # "Idle"
                \* Goes on after the turn running, in its answer.
                THEN /\ status' = "active" /\ turns' = 0
                     /\ Answer(a, i)
                     /\ UNCHANGED <<m, has, check, verified, llm, chk, open, cur,
                                    badTurn, lostWork>>
                ELSE /\ Answer(a, i)
                     /\ UNCHANGED <<m, has, status, turns, check, verified, llm,
                                    chk, open, cur, badTurn, lostWork>>
            [] e = "cancel" ->
                \* The turn is cut. The machine ends a turn it was in, once
                \* (`Aborted`), whatever was in flight; the surface closed
                \* that answer already.
                /\ status' = IF Active THEN "paused" ELSE status
                /\ m' = "Idle" /\ llm' = "none" /\ chk' = "none"
                /\ outbox' = IF MachineEndsTurn /\ m # "Idle"
                             THEN Append(outbox, [cont |-> FALSE, id |-> cur])
                             ELSE outbox
                /\ UNCHANGED <<has, turns, check, verified, open, cur,
                               badTurn, lostWork>>
            [] e = "continue" ->
                \* A message: the kernel starts the turn it asks for.
                IF m = "Idle"
                THEN /\ StartGoalTurn(Active, i)
                     /\ UNCHANGED <<has, status, turns, check, verified, chk,
                                    outbox, open>>
                ELSE UNCHANGED <<m, has, status, turns, check, verified, llm,
                                 chk, outbox, open, cur, badTurn, lostWork>>
            [] e \in {"done", "working"} ->
                IF m # "Gen"
                THEN UNCHANGED <<m, has, status, turns, check, verified, llm,
                                 chk, outbox, open, cur, badTurn, lostWork>>
                ELSE IF Active
                THEN /\ Settle(e = "done") /\ UNCHANGED <<has, check, open>>
                ELSE /\ Rest(FALSE)
                     /\ UNCHANGED <<has, status, turns, check, verified, llm,
                                    chk, open, cur, badTurn, lostWork>>
            [] e \in {"pass", "fail"} ->
                IF m # "Check"
                THEN UNCHANGED <<m, has, status, turns, check, verified, llm,
                                 chk, outbox, open, cur, badTurn, lostWork>>
                ELSE IF e = "pass" /\ has
                THEN /\ status' = "completed" /\ verified' = TRUE
                     /\ Rest(FALSE)
                     /\ UNCHANGED <<has, turns, check, llm, chk, open, cur,
                                    badTurn, lostWork>>
                ELSE IF Active /\ turns < MaxTurns /\ KernelContinues
                THEN /\ StartGoalTurn(Active, cur)
                     /\ UNCHANGED <<has, status, turns, check, verified, chk,
                                    outbox, open>>
                ELSE IF Active /\ turns < MaxTurns
                THEN /\ Rest(TRUE)
                     /\ UNCHANGED <<has, status, turns, check, verified, llm,
                                    chk, open, cur, badTurn, lostWork>>
                ELSE /\ status' = IF Active THEN "budget" ELSE status
                     /\ Rest(FALSE)
                     /\ UNCHANGED <<has, turns, check, verified, llm, chk,
                                    open, cur, badTurn, lostWork>>
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
