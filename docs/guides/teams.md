# Teams and Tasks

A **team** is a set of sven processes that share a task board on disk. Each
member polls the board, claims the tasks addressed to it (or to nobody), runs
each as a headless agent, and writes the outcome back. Teams are a feature of
the `network` cargo feature, which is on by default; the surface is the
`sven team` subcommand and the member process it starts.

Use a team when parts of a problem can run in parallel, or when different
parts want different models, instructions or tool restrictions. For a single
focused sub-task inside one session, use the `task` tool instead (see
[delegation](../concepts/delegation.md)).

---

## Tasks

A task is a unit of work with:

- a **title** and a **description** (the full context and the definition of
  done),
- a **status**: `pending`, `in_progress`, `completed`, `failed` or
  `cancelled`,
- an optional **assignee**: a member name. A task with no assignee can be
  claimed by any member,
- optional **dependencies** on other task ids: a task cannot be claimed until
  all of them are `completed`.

The board is `~/.config/sven/teams/<team>/tasks.json` (the platform config
directory; `~/.config` on Linux): `{"tasks": [...]}`, with each task carrying
`id`, `title`, `description`, `status` (`{"state": "pending"}`, or
`in_progress`/`completed`/`failed` with their fields), `assigned_to`,
`depends_on`, `created_by`, `created_at` and `updated_at`. Every mutation by a
sven process takes an exclusive file lock, so members claim tasks without
races. Edit the file by hand only while no member is running.

```
pending --claim--> in_progress --run succeeds--> completed
                        |
                        +-------run ends otherwise or errors--> failed
```

A member claims the first pending task whose dependencies are done and that is
assigned to it or to nobody. The member (not the model) marks the task:
`completed` with the agent's final reply as the summary, or `failed` with the
reason when the run did not conclude successfully or errored.

---

## Defining and starting a team

Team definitions are YAML files in `.sven/teams/` of the project:

```yaml
# .sven/teams/release.yaml
name: release-prep
goal: Prepare the project for a version release
max_active: 4              # default 8

members:
  - role: explorer
    name: changelog-writer
    model: anthropic/claude-sonnet-4-5   # optional; default: the configured model
    instructions: |
      Read the git log since the last tag and write a CHANGELOG.md entry.
      Focus on user-facing changes only.

  - role: reviewer
    name: pr-reviewer
    deny_tools: [write_file, edit_file]
    instructions: |
      Read the diff since the last tag and write a review covering
      correctness, edge cases and regressions.

token_budget: 500000   # tokens across the whole team; 0 = unlimited (default)
max_iterations: 40     # tool rounds per task run; 0 = the configured default
```

`role` is one of `implementer`, `reviewer`, `explorer`, `tester`, `teammate`
(the default) or any other string, which is kept as a label. The role appears
in the roster and in the member's prompt; it does not change what the member
may do. `model` and `instructions` apply to every task the member runs;
`deny_tools` lists tools the member never sees nor runs.

### The `sven team` subcommands

| Command | What it does |
|---|---|
| `sven team init --name N [--goal G]` | Write a starter `.sven/teams/N.yaml` |
| `sven team definitions` | List the definition files found in the project |
| `sven team start --file F [--sven-bin PATH] [--dry-run]` | Create the team (or update it from the file) and start one `sven` process per member; `--dry-run` prints the commands without writing the team or starting anything |
| `sven team create --name N [--goal G] [--max-active 8] [--token-budget 0]` | Create the team directory and config without starting members; the name is alphanumeric, `-` and `_` |
| `sven team list` | Teams on disk with member and active counts |
| `sven team status NAME` | Members, their status and current task, token budget use, task counts |
| `sven team watch NAME [--interval 3] [--timeout 0]` | Poll a team; status lines go to stderr and JSON event lines (`member_status`, `task_summary`, `token_budget_warning` at 90% of the budget, `team_complete`) to stdout. Exits once every task is completed, or after `--timeout` seconds (0 = never) |
| `sven team cleanup NAME [--force]` | Remove the team directory; without `--force` it only prints the status and removes nothing |

`sven team start` writes the goal and the limits (`max_active`, `token_budget`,
`max_iterations`, each member's `deny_tools` and `model`) to the team config
before any member starts. Re-running it on an existing team updates the
config and keeps the members' runtime state.

### Putting work on the board

`sven team start` creates the members and an empty board; members then wait.
Work gets onto the board in one of two ways:

- Write tasks into `tasks.json` (see above) before starting members.
- Start a member with a prompt: the prompt is recorded once as a task titled
  `Initial task`, assigned to that member. The member flags are
  `--team-name`, `--team-role` and `--teammate-name` (alias `--agent-name`),
  hidden from `--help`; `sven team start` passes them for you.

Hosts that embed sven can register the team tools listed below so a lead agent
manages the board itself. The stock `sven` binary does not register them, so
the interactive agent has no `create_team`, `create_task` or similar tool.

---

## What a member does

A member process runs headless and never waits for a person. It registers
itself in the team config, then loops:

1. Stop if its status in the team config is `closed`, if it was removed from
   the config, or if the team directory is gone.
2. Read its terms (denied tools, tool-round limit) from the team config.
3. Reserve its share of the remaining token budget; stop if the budget is
   spent.
4. Claim the next task. With none available, sleep five seconds and look
   again. A member does not exit when the board is empty; it exits on one of
   the conditions in step 1 or 3.
5. Run the task as an agent with the coding toolset and the member's model and
   instructions, settle the token reservation with what the run used, and
   mark the task `completed` or `failed`.

If the team config stays unreadable across several retries with growing
delays, the member exits with an error.

The limits are enforced per member from the team config:

- `deny_tools` - tools the member's task runs never see nor run.
- `max_iterations` - the tool-round limit of each task run (it replaces
  `agent.max_tool_rounds` for the member).
- `token_budget` - input and output tokens across every member's task runs
  and their sub-agents. Before each task a member reserves its share of what
  is left, split evenly between the members working, under the team-config
  lock, so members running at once never overshoot together. The run's output
  is held to that share, everything it used (input and output, its
  sub-agents' included) replaces the reservation afterwards, whether or not it
  succeeded, and once the budget is spent a member claims no further task and
  exits.

A member runs under auto approval: every call its terms allow runs without a
prompt (a denied tool is never offered), its sub-agents' included, and a
question it asks is answered at once with "No user is available to answer this
question. Proceed on your best judgement and state the assumption you made."

---

## Roles

| Role | Typical use |
|---|---|
| `explorer` | Investigate a codebase area, gather information, write notes |
| `implementer` | Write or change code based on a specification |
| `reviewer` | Read code and produce a review report |
| `tester` | Run tests, analyse failures |
| `teammate` | General purpose |

A member started by `sven team start` with an unrecognised role runs as
`teammate`.

---

## Team tools

The `sven-team` crate provides these as `sven_tool_api::Tool` implementations
for hosts that register them. The first group is for the lead.

| Tool | What it does |
|---|---|
| `create_team` | Initialise a team (or restore it if it exists) and become its lead |
| `load_team` | Re-attach to a team that exists on disk |
| `list_team` | Members with role, status and current task; `show_closed` includes closed members |
| `spawn_teammate` | Start a member process (`name`, `role`, `model`, `task_prompt`, `use_worktree`); lead only |
| `shutdown_teammate` | Mark a member `closed` by `name` or `peer_id`; it finishes its current task and exits |
| `register_teammate` | Register an already-running process as a member |
| `read_teammate_log` | Tail a spawned member's log, `~/.config/sven/teams/<team>/<name>.log` |
| `merge_teammate_branch` | Merge a member's worktree branch into the current branch with `git merge --no-ff` |
| `cleanup_team` | Remove the team directory; refuses while members are active unless `force` is true; lead only |
| `create_task` | Add a task (`title`, `description`, `assigned_to`, `depends_on`) |
| `list_tasks` | Show tasks with status, assignee, summaries and `[blocked: n/m deps done]` markers; optional status filter |
| `assign_task` | Direct a task to a member |
| `claim_task` | Mark a task `in_progress` for the caller |
| `complete_task` | Mark a task `completed` with a summary |
| `update_task` | Replace a task's description |

### Git worktree isolation

`spawn_teammate` with `use_worktree: true` creates the branch
`sven/team-<team>/<role>-<name>` from `HEAD` and a worktree at
`<repo>/.sven-worktrees/<team>/<role>-<name>` for that member, so concurrent
edits do not collide. The current directory must be inside a Git repository.
`merge_teammate_branch` merges the branch back. Members started with
`sven team start` share the directory they are started in.

---

## Recursion and safety

sven enforces hard limits to prevent runaway agent chains:

- **Subprocess depth** - a sub-agent started by the `task` tool cannot itself
  start further sub-agents.
- **`spawn_teammate`** - the library tool refuses unless the caller is the
  team lead, so a teammate cannot spawn sub-teams.

A `task` sub-agent never holds more than the session that started it:

- It runs in an ACP mode whose own policy - what its tools do without asking
  included - stays within what the parent may do in every state: an `agent`
  session may start any child, a `research`, `plan` or `chat` session only
  `research`/`plan` children, and an SDLC session none. If the child will not
  switch to that mode, it is stopped rather than left in its default mode.
- A permission request it sends is held to what the parent may do: the
  request names the capability the sub-agent classed the call under (a
  request that does not is held to what its tool's name says, but never
  taken for a read; an unknown tool, such as an MCP one, counts as network
  access, as it does in the kernel), and a call the parent's mode
  does not allow - a write under a `research` parent, say - is refused
  without asking anyone. Within that, the call runs without a prompt, as the
  parent's own would; under the parent's manual approval (`--approval
  manual`) every call of the sub-agent that is not read-only - a web fetch or
  a memory write as much as a shell command - is put to the parent's own
  approval prompt (tool and command or path), and under an IDE driving the
  parent over ACP it is put to the IDE. The sub-agent is started with the
  parent's approval mode and `tools.deny_patterns`/`auto_approve_patterns`.
- It never runs a tool the parent session has disabled (`tools.disabled`,
  passed on as `--disable-tool`), takes at most the parent's
  `agent.max_tool_rounds` tool rounds a turn, and writes at most the parent's
  configured output cap per response, never more than its own model allows.
- It is stopped after `agent.child_run_timeout_secs` (default one hour) of
  wall-clock time, however busy, and after 10 minutes without any output -
  a pending permission request pauses that count, and the child waits for
  the answer until its deadline (or, with no deadline, for its 60-second
  permission timeout), after which the call is denied. Exiting before its turn finished, or at its
  deadline, is reported as a failure.
- A request of its still waiting when it stops is taken off the parent's
  screen.
- The tokens it used are reported with its turn and charged where the
  parent's are (a team member's budget, for one).

These limits are enforced at the system level, not by the model.  No prompt
can override them.

---

## Monitoring without a running session

The board and the team config are plain files:

```sh
python3 -m json.tool ~/.config/sven/teams/release-prep/tasks.json
python3 -m json.tool ~/.config/sven/teams/release-prep/config.json
```

`sven team status` and `sven team watch` read the same files.

---

## Troubleshooting

### A member is running but claims nothing

A member claims only tasks that are pending, whose dependencies are completed,
and that are assigned to its name or to nobody. Check `tasks.json` for the
assignee spelling and for dependency ids that are not `completed`.

### A task stays `in_progress`

The member that claimed it may have been killed. `sven team status` shows the
member's status. There is no command to release the claim; edit the task back
to `{"state": "pending"}` in `tasks.json` while no member is running.

### A member exits at once

It exits when the team's token budget is spent, when the team directory
or its entry in the team config is gone, or when its status is `closed`. Its
output goes nowhere when started by `sven team start`; run the same command
that `--dry-run` prints in a terminal to see its log lines.

### A team directory is left behind

`sven team cleanup NAME --force`, or remove
`~/.config/sven/teams/<team>` by hand.
