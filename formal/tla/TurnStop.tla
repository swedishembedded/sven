----------------------------- MODULE TurnStop -----------------------------
\* SPDX-License-Identifier: Apache-2.0
\* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
\*
\* A turn the person stops ends, once, and starts nothing more.
\*
\* This models `crates/kernel/src/erased.rs`'s dispatch loop together with
\* `crates/kernel/src/cancel.rs::TurnScope` and the way a surface stops a
\* turn (`crates/bootstrap/src/session_handles.rs::cancel`): the turn's scope
\* is cancelled first, which stops the work in flight (a spawned tool call, a
\* model stream), and `Event::UserCancelled` is posted after it, which the
\* machine answers by ending the turn and announcing so
\* (`crates/machines/src/machines/reactive_agent.rs::cut`). Every effect of a
\* dispatch runs under the turn scope current at that dispatch; a cancelled
\* scope drops them.
\*
\* The race is between the stop's event and the results of the work it
\* stopped, or of work that finished just before it: any of them can be
\* dispatched first. Two choices decide what survives the stop.
\*
\* When a stopped scope is renewed, so that what comes after the stop runs:
\*
\*   Renew = "AfterDispatch"        -- only after a dispatch. The stop's own
\*                                     dispatch, arriving first, then runs
\*                                     under the scope it stopped, and its
\*                                     announcement is dropped: the turn
\*                                     never ends for the surface, which holds
\*                                     the next message behind it. This was
\*                                     the kernel until 2026-11-15.
\*   Renew = "OnStop"               -- also before the stop's own dispatch.
\*                                     The kernel as written. MUST pass.
\*   Renew = "BeforeEveryDispatch"  -- before any dispatch. The stopped
\*                                     call's result then runs its
\*                                     continuation, a model call nobody
\*                                     asked for after the stop.
\*
\* Whether a dispatch's announcement that the turn ended runs under a
\* cancelled scope (`AnnounceSurvivesStop`): the ending is not the turn's
\* work, it is what the surface waits on. Without it, a model's final reply
\* already queued when the stop comes ends the turn in the machine, its
\* announcement is dropped, and the stop then finds the machine at rest,
\* with nothing to end: the turn never ends for the surface. TRUE is the
\* kernel as written.
\*
\* The stop also pauses a goal the turn worked on (`PauseGoal`, an effect of
\* its own dispatch, at rest too); that is what renewing on the stop keeps.
\*
\* Swedish Embedded AB implements solutions for agent runtimes whose stop
\* button always works for its clients. If your team needs expertise in
\* cancellation in event-driven systems then you can procure our services by
\* sending an email to info@swedishembedded.com.

EXTENDS Naturals, Sequences

CONSTANTS Renew, AnnounceSurvivesStop

Renews == {"AfterDispatch", "OnStop", "BeforeEveryDispatch"}
ASSUME Renew \in Renews
ASSUME AnnounceSurvivesStop \in BOOLEAN

Events == {"ToolResult", "ToolFailed", "LlmDone", "UserCancelled"}

VARIABLES
  machine,     \* the machine's state: "Idle" or "Generating"
  scope,       \* the current turn scope: "live" or "cancelled"
  queue,       \* the kernel's event queue
  tool,        \* the turn's tool call: "none", "running" or "done"
  llm,         \* the turn's model call: "none", "running" or "done"
  stop,        \* the person's stop: "none", "cancelled" (scope stopped,
               \* event not posted yet) or "posted"
  ended,       \* times the surface was told the turn ended
  llmAfterStop, \* a model call started for the turn after the stop
  goalPaused   \* the stop paused the turn's goal

vars == <<machine, scope, queue, tool, llm, stop, ended, llmAfterStop, goalPaused>>

TypeOK ==
  /\ machine \in {"Idle", "Generating"}
  /\ scope \in {"live", "cancelled"}
  /\ queue \in Seq(Events)
  /\ tool \in {"none", "running", "done"}
  /\ llm \in {"none", "running", "done"}
  /\ stop \in {"none", "cancelled", "posted"}
  /\ ended \in 0..3
  /\ llmAfterStop \in BOOLEAN
  /\ goalPaused \in BOOLEAN

\* The person's message has started a turn whose first step is a tool call.
Init ==
  /\ machine = "Generating"
  /\ scope = "live"
  /\ queue = <<>>
  /\ tool = "running"
  /\ llm = "none"
  /\ stop = "none"
  /\ ended = 0
  /\ llmAfterStop = FALSE
  /\ goalPaused = FALSE

\* ---- the outside world -------------------------------------------------

ToolFinishes ==
  /\ tool = "running" /\ scope = "live"
  /\ tool' = "done" /\ queue' = Append(queue, "ToolResult")
  /\ UNCHANGED <<machine, scope, llm, stop, ended, llmAfterStop, goalPaused>>

\* The executor's select sees the cancelled scope and answers the call.
ToolStopped ==
  /\ tool = "running" /\ scope = "cancelled"
  /\ tool' = "done" /\ queue' = Append(queue, "ToolFailed")
  /\ UNCHANGED <<machine, scope, llm, stop, ended, llmAfterStop, goalPaused>>

LlmReplies ==
  /\ llm = "running" /\ scope = "live"
  /\ llm' = "done" /\ queue' = Append(queue, "LlmDone")
  /\ UNCHANGED <<machine, scope, tool, stop, ended, llmAfterStop, goalPaused>>

\* A stopped stream ends with no result: the machine ends the turn.
LlmStopped ==
  /\ llm = "running" /\ scope = "cancelled"
  /\ llm' = "done"
  /\ UNCHANGED <<machine, scope, queue, tool, stop, ended, llmAfterStop, goalPaused>>

\* Stop, in its two steps: the scope first, then the event.
StopCancels ==
  /\ stop = "none" /\ machine = "Generating"
  /\ stop' = "cancelled" /\ scope' = "cancelled"
  /\ UNCHANGED <<machine, queue, tool, llm, ended, llmAfterStop, goalPaused>>

StopPosts ==
  /\ stop = "cancelled"
  /\ stop' = "posted" /\ queue' = Append(queue, "UserCancelled")
  /\ UNCHANGED <<machine, scope, tool, llm, ended, llmAfterStop, goalPaused>>

\* ---- the kernel ----------------------------------------------------------

\* What the machine does with `e` in `m`: its next state and its effects
\* ("CallLlm" starts the continuation, "Announce" tells the surface the turn
\* ended, "PauseGoal" pauses the goal the stopped turn worked on). A stop at
\* rest pauses the goal and announces nothing: no turn is running to end.
Next_(m, e) ==
  IF m = "Generating" /\ e \in {"ToolResult", "ToolFailed"} THEN [to |-> "Generating", fx |-> {"CallLlm"}]
  ELSE IF m = "Generating" /\ e = "LlmDone" THEN [to |-> "Idle", fx |-> {"Announce"}]
  ELSE IF m = "Generating" /\ e = "UserCancelled" THEN [to |-> "Idle", fx |-> {"Announce", "PauseGoal"}]
  ELSE IF m = "Idle" /\ e = "UserCancelled" THEN [to |-> "Idle", fx |-> {"PauseGoal"}]
  ELSE [to |-> m, fx |-> {}]

\* One dispatch: renew where the design renews before it, react, run the
\* effects under the scope current now (dropped when it is cancelled), and
\* renew a stopped scope after it.
Dispatch ==
  /\ queue # <<>>
  /\ LET e == Head(queue)
         before == IF \/ Renew = "BeforeEveryDispatch"
                      \/ (Renew = "OnStop" /\ e = "UserCancelled")
                   THEN "live" ELSE scope
         r == Next_(machine, e)
         runs == before = "live"
         announces == "Announce" \in r.fx /\ (runs \/ AnnounceSurvivesStop)
     IN /\ queue' = Tail(queue)
        /\ machine' = r.to
        /\ ended' = IF announces THEN ended + 1 ELSE ended
        /\ llm' = IF runs /\ "CallLlm" \in r.fx THEN "running" ELSE llm
        /\ llmAfterStop' = (llmAfterStop \/ (runs /\ "CallLlm" \in r.fx /\ stop # "none"))
        /\ goalPaused' = (goalPaused \/ (runs /\ "PauseGoal" \in r.fx))
        /\ scope' = "live"
  /\ UNCHANGED <<tool, stop>>

Next ==
  \/ ToolFinishes \/ ToolStopped \/ LlmReplies \/ LlmStopped
  \/ StopCancels \/ StopPosts \/ Dispatch

Spec == Init /\ [][Next]_vars

\* ---- properties ----------------------------------------------------------

\* Nothing is left to happen: no event queued, no work running, the stop
\* (if any) delivered.
Quiet ==
  /\ queue = <<>>
  /\ tool # "running" /\ llm # "running"
  /\ stop # "cancelled"
  /\ ~ENABLED Next

\* Every turn ends, for the surface, exactly once: a stopped one too.
EveryTurnEndsOnce == /\ ended <= 1
                     /\ Quiet => ended = 1

\* A stopped turn asks the model nothing more.
NoModelCallAfterStop == ~llmAfterStop

\* A stop pauses the goal, wherever it finds the machine.
StopPausesGoal == (Quiet /\ stop = "posted") => goalPaused
=============================================================================
