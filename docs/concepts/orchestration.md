<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Orchestration

Swedish Embedded AB implements solutions for splitting software work into
independent pieces that coding agents build in parallel for its clients. If
your team needs expertise in agent orchestration then you can procure our
services by sending an email to info@swedishembedded.com.

A model left to itself rarely delegates, so a long build runs as one agent
from start to end. With `agent.orchestration: plan` the main agent is offered
`plan_tasks`: planning becomes an explicit step that sven checks, and the
independent parts run in parallel. The default, `off`, offers no such tool and
changes nothing.

Orchestration is built from parts that exist already: the writing children and
their [isolated worktrees](delegation.md#isolated-writing-children) of the
`task` tool, and the verification of a [goal](../guides/goals.md). Deciding
the split is the model's; running it is deterministic code.

## The task graph

`plan_tasks` takes a list of tasks and, optionally, a `verify_command`. A task
has:

| Field | Meaning |
|-------|---------|
| `id` | Unique; letters, digits, `_`, `-`, `.` |
| `title` | A label (defaults to the id) |
| `prompt` | The child's whole brief. It sees nothing else of the conversation |
| `owns` | Relative files, directories or globs only this task writes |
| `acceptance` | How the task is known to be done |
| `depends_on` | Ids that must be integrated before this task starts |

sven refuses a graph with every violation named, runs nothing, and the model
submits a corrected one. The rules:

- ids are unique, every `depends_on` names a task, and there is no cycle;
- every task has a non-empty `prompt` and at least one `owns` entry, relative
  and inside the project;
- at most `orchestration_max_tasks` tasks, and at most
  `orchestration_max_fanout` of them in the widest layer (tasks that could run
  side by side);
- two tasks with no dependency path between them, which could run at the same
  time, own no file in common. A plain path owns everything beneath it; two
  globs are compared by where they are rooted, so the check errs toward
  reporting an overlap. Order such tasks with `depends_on`.

A refused graph costs nothing against the session's graph budget.

## Running it

Tasks start as soon as everything they depend on has been integrated, at most
`orchestration_max_parallel` at a time. Each runs as a writing `task` child
with `isolation: "worktree"`, so it starts from the parent's tree as it is at
that moment, including what earlier tasks added, and each finished change is
merged by the same serialized three-way integration as for any isolated child.
The children are held to the parent's authority exactly as `task` children are
and cannot plan or spawn. Their brief names the files they own and asks them
to stay inside them.

A task ends as one of:

| Status | Meaning |
|--------|---------|
| `integrated` | the change is in the parent's tree (uncommitted) |
| `conflict` | it overlaps a change already integrated; nothing was applied and its branch is kept and named |
| `failed` | the child failed or ran past `agent.child_run_timeout_secs`; nothing was applied, the branch is named when anything was saved |
| `skipped` | not started, because a task it depends on did not integrate (transitively) |

A child past its time is failed and never integrated late. Cancelling the
parent cancels the children and removes their worktrees.

## The result and verification

The model gets one JSON result: a one-line summary, then per task its status,
files changed, kept branch, the child's report (cut at 3000 characters),
provider-reported tokens when known, and wall time; `graphs_left`; and `next`,
which says whether anything is left to repair.

A child's report is its own claim. When the plan sets `verify_command`, sven
runs it after the graph, through the session's shell tool (so
`tools.deny_patterns` and the shell's limits apply), and returns `passed` and
the tail of its output. On failure the model repairs the work itself or submits
a follow-up graph, bounded by `orchestration_max_graphs` per session. With no
`verify_command` the result says nothing verified the work. Under a standing
goal (`/goal --check`, `--goal-check`) the goal's check still decides when the
goal is done; a plan does not read it, so give the same command as
`verify_command` for the model to see failures early.

## Configuration

```yaml
agent:
  orchestration: plan            # off (default) | plan
  orchestration_max_parallel: 4  # children running at once
  orchestration_max_fanout: 8    # widest layer of a graph
  orchestration_max_tasks: 24    # tasks in one graph
  orchestration_max_graphs: 3    # graphs one session may run
```

`--orchestration off|plan` overrides `agent.orchestration` for one process. When
it is `plan`, the system prompt gains one short paragraph on when to use the
tool: independent modules or files that can be built in parallel; small tasks
directly. A session without the tool (a sub-agent, a confined shell, a
custom system prompt) has no such paragraph.

## What a trajectory records

The graph is the arguments of the `plan_tasks` call and the per-task statuses
and verification are its result, both in the trajectory. Each child's
trajectory is embedded in `subagent_trajectories`, tied to the call, as for
`task` children.

## Limits

- Tasks run on the machine of the parent, and integration is serialized per
  sven process.
- The graph budget is per session, not per turn.
- A task that edits files it does not own can still conflict; the conflict is
  then reported like any other.
- Integrated changes are left uncommitted in the parent's tree.
