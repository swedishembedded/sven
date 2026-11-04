<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Goals

A goal is an objective the session keeps working toward on its own: you set
it once with `/goal`, and sven takes turn after turn until the goal is met,
something blocks it, you pause it, or its turns run out. You see one answer
for the whole run, which ends when the goal stops.

The model does the work; it does not decide when the work is done. A check
command you trust decides that, or, without one, the model's own report, which
sven then marks as unverified.

## Commands

| Command | What it does |
|---------|--------------|
| `/goal <objective>` | Set the goal and start working on it now, with 20 turns. Replaces any goal there was. Only at rest: while a turn runs, wait for it or stop it first. |
| `/goal --turns N <objective>` | The same with `N` turns (at most 500). |
| `/goal --check "<command>" <objective>` | The same, decided by that command (see `/goal check`). The flags go before the objective, in any order. |
| `/goal` or `/goal status` | Show the goal: its status, turns, check and last result, and what blocks it. Without a goal: `No goal is set. Set one using /goal command`. |
| `/goal check [--timeout SECS] <command...>` | Decide completion with a command from now on: the goal is met when it exits with status 0 within its time (default 600 seconds). It is a program and its arguments, run in the project without a shell of its own; quote an argument that holds spaces. |
| `/goal check off` | Remove the check; the model's report decides again. |
| `/goal pause` | Take no further turns. A turn that is running finishes. |
| `/goal resume` | Continue a paused, blocked or out-of-turns goal, with a fresh set of turns. |
| `/goal clear` | Drop the goal. A turn that is running finishes as an ordinary answer. |

Stopping the running turn (`Esc`, `/abort`, or stopping the answer from a
server) pauses the goal; `/goal resume` continues it.

You can also ask the agent to set the goal for you: "set the most
appropriately worded goal for this session". It words the objective (and a
check, when a command can decide it) with its `set_goal` tool, and the goal
starts when its reply ends, in the same answer. The agent sets a goal only in
a turn you started: during the goal's own turns it cannot change what it is
working toward, and a session that asks before tool calls asks you first.

## How a goal runs

Each turn starts with an instruction that holds the objective as you wrote
it, how completion is decided, and what the last turn and check showed. After
every turn that settles:

1. With a check set, the check runs. Only a passing check completes the goal,
   and it is marked **verified**. A check that cannot run or reaches no
   verdict pauses the goal; it never counts as a pass.
2. Without a check, the goal completes when the model reports it complete
   with the `update_goal` tool and says what shows it. It is marked
   **unverified**.
3. The model can report the goal blocked (a permission, a credential, a
   service, a decision only you can make). One report is not enough: the goal
   stops as **blocked** only when the same blocker is reported on three turns
   in a row, so the model first looks for another way. Its report says what
   fails, the evidence, what is done, what remains, what it needs from you and
   what must be true to resume.
4. Three turns in a row without any action (no tool call, no report) stop the
   goal as **blocked** with a report sven writes itself.
5. When its turns run out, the goal stops as **out of turns**, never as done.
6. Otherwise the next turn starts, with what the check printed when it
   failed.

Repeating work is not a reason to stop: running the same failing test after
every change is how a fix gets made, and only the rules above end a goal.

A goal never widens what the session may do. Its turns and its check go
through the same permission gate as any other turn: the check needs the shell
capability, and a session that asks before running commands asks for the
tools the model calls. The model is told never to work around a permission;
a refused one is something to report.

## Where goals work

- **The terminal** (`sven`): the status line shows what changed (`goal: ...`)
  and `/goal` shows the details. The goal is saved with the session, and a
  resumed session gets it back.
- **`sven rc`**: `/goal` is one of the commands a connected server can run
  ([Remote control](remote-control.md)). The goal is kept beside the
  conversation, so a host that restarts gets it back.

A goal that was running when its session stopped (a restart, a resumed
session, a change of mode or model) comes back **paused**: the work it was
doing may or may not have taken effect, so nothing runs again until you say
`/goal resume`.

The `sdlc` mode keeps no goal. Headless runs and editors (ACP) show goal
changes but do not take `/goal` commands.

## For integrators

Every change of the goal is a `GoalUpdated` session event: the goal as it
stands (or `null` once cleared) and a sentence saying what happened. It is
sent when a goal is set, takes a turn, is checked, changes status or is
cleared, and in answer to every `/goal` command, so a surface shows what it
last received.

```json
{"GoalUpdated": {
  "goal": {
    "id": "goal-1",
    "objective": "make the parser tests pass",
    "status": "active",
    "check": {"argv": ["cargo", "test"], "timeout_secs": 600},
    "last_check": {"outcome": "failed", "detail": "..."},
    "turns_used": 2,
    "max_turns": 20,
    "verified": false
  },
  "note": "The check fails; turn 3 of 20."
}}
```

`status` is one of `active`, `paused`, `blocked`, `completed` and
`budget_exhausted`; a blocked goal carries `blocker` with the fields of the
report, and a stopped one `reason`.

A goal command sent with no answer open is the person's turn: it ends with
`TurnComplete`, or, when it starts work, with the end of that work. A command
sent while an answer is open (`/goal pause` during a goal) only changes what
follows: it never starts work and ends nothing, even if the turn it was sent
during has finished by the time it arrives - setting or resuming a goal that
way is refused, and says so.
The machine's side is described in
[State machines](../concepts/state-machines.md).
