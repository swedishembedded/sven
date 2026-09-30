# Teams and Tasks

When a task is too large for one agent to handle cleanly - or when you want
different parts of a problem to run in parallel - sven supports **teams**: a
lead agent that creates and assigns work, and one or more teammate agents that
pick up and complete tasks independently.

This page explains the team model from the ground up: what a team is, how to
create one, how tasks flow through it, and how to prompt the agent effectively
at each step.

---

## The mental model

Think of a sven team like a small software contractor arrangement:

- The **lead** is you at your desk, breaking a project into tickets.
- Each **teammate** is a developer who takes a ticket, does the work, and
  reports back.
- The **task list** is a shared board that everyone reads from and writes to.

The key difference from a real team: every conversation, decision, and action
is captured in the agent's tool calls and responses, so you can inspect exactly
what happened at any point.

---

## How tasks work

A task is a unit of work with:

- A **title** (short, one line)
- A **description** (full context: what to do, how to know it is done)
- A **status**: `pending` → `in_progress` → `completed` (or `failed`)
- An optional **assigned agent** (if omitted, any teammate can claim it)
- Optional **dependencies** on other tasks (a task cannot be claimed until its
  dependencies are all completed)

Tasks are stored on disk under `~/.config/sven/teams/<team-name>/tasks.json`.
They survive restarts and can be inspected by hand.

### Task lifecycle

```
create_task  →  pending
                   │
          assign_task (optional)
          claim_task  →  in_progress
                              │
                       complete_task  →  completed
                       (or mark failed if something went wrong)
```

The lead typically creates all the tasks upfront.  Teammates then either
self-claim the next available task (`claim_task` with no `task_id`) or are
directed to a specific one.  When a teammate finishes, it calls `complete_task`
with a summary.  The lead can call `list_tasks` at any time to see the board.

---

## What you actually do

Teams work best when you let the agent manage the process.  Your job is to
write one good prompt.  The agent handles everything else - creating the team,
spawning teammates, creating tasks, assigning them, tracking completion, and
reporting results back to you.

### The simplest approach: one prompt

```
sven "Refactor the auth module in src/auth/ to use PASETO instead of JWT.

Use a team:
1. Create a team called 'auth-refactor'.
2. Spawn an explorer teammate to read the current auth code and write a
   migration plan to plan.md.
3. Spawn an implementer teammate to carry out the plan once it exists.
4. Use list_tasks to monitor progress.
5. When both tasks are complete, summarise what changed."
```

The agent will:
- Call `create_team` to initialise the shared task store
- Call `spawn_teammate` twice - once for the explorer, once for the implementer
- Call `create_task` for each piece of work
- Monitor with `list_tasks` until everything is completed
- Report back to you

---

## Interactive walkthrough

The following prompts show how to drive a team step by step, as you would type
them into an interactive sven session.

### Step 1: Create the team

```
Create a team called 'release-prep' with the goal of preparing version 2.0 for
release. You are the lead.
```

The agent calls `create_team` and confirms:

```
Team 'release-prep' created. You are the lead.
Next steps:
  1. Use spawn_teammate to add teammates with specific roles
  2. Use create_task to define work items
  ...
```

### Step 2: Spawn teammates

```
Spawn two teammates:
- An 'explorer' named 'changelog-writer' whose job is to read git log and
  write a CHANGELOG.md entry for v2.0.
- A 'tester' named 'test-runner' whose job is to run the full test suite and
  report failures.
```

The agent calls `spawn_teammate` for each.  Each spawned agent starts as a
separate process and connects to the same team.  They will appear in
`list_team` once connected.

### Step 3: Create tasks

```
Create the following tasks:
1. Title: 'Write CHANGELOG entry'
   Description: Read git log since v1.9.0, extract meaningful changes,
   write a CHANGELOG.md entry under '## 2.0.0'. Assign to changelog-writer.

2. Title: 'Run test suite'
   Description: Run 'cargo test --workspace'. If any tests fail, list the
   test names and the error messages. Assign to test-runner.
```

The agent calls `create_task` twice.  Both tasks appear on the shared board
immediately.

### Step 4: Monitor progress

```
Check the team status - who is working on what and how far along are the tasks?
```

The agent calls `list_team` and `list_tasks` and reports back:

```
Team 'release-prep' - 3 members | tasks: pending=0, in_progress=2, completed=0, failed=0

● changelog-writer [Explorer] - working on: "Write CHANGELOG entry"
● test-runner      [Tester]   - working on: "Run test suite"
○ you              [Lead]
```

### Step 5: Wait for completion and synthesise

```
Wait until all tasks are completed or failed, then summarise the results.
```

The agent polls `list_tasks` and, when all tasks are done, reads the summaries
and assembles them into a report.

### Step 6: Clean up

```
All tasks are done. Shut down the teammates and clean up the team.
```

The agent calls `shutdown_teammate` for each teammate and then `cleanup_team`
to remove the team directory.

---

## Roles

When spawning teammates, you assign each one a **role**.  The role is a hint to
the LLM about what the agent should focus on - it is injected into the system
prompt.

| Role | Typical use |
|---|---|
| `explorer` | Investigate a codebase area, gather information, write notes |
| `implementer` | Write or change code based on a specification |
| `reviewer` | Read code and produce a review report, suggest improvements |
| `tester` | Run tests, analyse failures, suggest fixes |
| `teammate` | General purpose - no specific focus |

You can also use any custom string: `role: "documentation-writer"` or
`role: "security-auditor"` work fine.

---

## How to direct teammate work

The **only** correct way to give a teammate work is through the task system.

| What you want | Correct tool |
|---|---|
| Give a teammate a specific piece of work | `create_task` with `assigned_to` |
| Re-point a task to a different teammate | `assign_task` |
| See what everyone is working on | `list_tasks` + `list_team` |

Prompt pattern that reliably works:

```
Create a task titled 'Write the CHANGELOG entry' with the description:
"Read git log since the last tag. Write a new section under '## 2.0.0'
in CHANGELOG.md covering only user-facing changes."
Assign it to changelog-writer.
Then call list_team and list_tasks to monitor progress.
```

The teammate picks up the task by calling `claim_task`, does the work, then
calls `complete_task` with a summary. You see the result in `list_tasks`.

---

## Working directories and Git isolation

By default all teammates work in the same directory as the lead.  For tasks
that involve editing files, you may want each teammate to work in its own Git
branch so changes do not conflict.

```
Spawn an implementer called 'auth-impl', use a Git worktree so it works in
its own isolated branch.
```

The agent calls `spawn_teammate` with `use_worktree: true`.  sven creates a
branch named `sven/team-<team>/<role>-<name>` and a worktree at
`.sven-worktrees/<name>`.  When the teammate finishes, the lead can merge the
branch:

```
Merge the auth-impl teammate's branch.
```

This calls `merge_teammate_branch` with a non-fast-forward merge commit.

---

## Task dependencies

When one task cannot start until another finishes, declare a dependency:

```
Create a task 'Deploy to staging' that depends on the 'Run test suite' task.
The deployment should only proceed once all tests pass.
```

The agent creates the deployment task with `depends_on: ["<test-suite-id>"]`.
When a teammate tries to claim it, sven automatically blocks it until the test
suite task is completed.  `list_tasks` shows blocked tasks clearly:

```
○ [d8f3] Deploy to staging [blocked: 0/1 deps done]
```

---

## Defining teams in YAML

For work you repeat often, define the team structure in a YAML file and start
it with a single command.  sven reads `.sven/teams/*.yaml`:

```yaml
# .sven/teams/release.yaml
name: release-prep
goal: Prepare the project for a version release
max_active: 4

members:
  - role: explorer
    name: changelog-writer
    instructions: |
      Read the git log since the last tag and write a CHANGELOG.md entry.
      Focus on user-facing changes only.

  - role: tester
    name: test-runner
    instructions: |
      Run the full test suite. Report any failures with test names and
      error messages. Do not attempt fixes - only report.

  - role: reviewer
    name: pr-reviewer
    deny_tools: [write_file, edit_file]
    instructions: |
      Read the diff since the last tag. Write a review covering correctness,
      edge cases, and any potential regressions.

token_budget: 500000   # tokens across the whole team; 0 = unlimited
max_iterations: 40     # tool rounds per task run; 0 = the configured default
```

Start it:

```sh
sven team start --file .sven/teams/release.yaml
```

This creates the team (or updates it, if it exists) and spawns all members
automatically.  No prompting needed.

The limits are written to the team config before any member starts, and each
member reads its own before every task:

- `deny_tools` - tools the member's task runs never see nor run.
- `max_iterations` - the tool-round limit of each task run (it replaces
  `agent.max_tool_rounds` for the member).
- `token_budget` - input and output tokens across every member's task runs
  and their sub-agents. Before each task a member reserves its share of what
  is left - split evenly between the members working, under the team-config
  lock, so members running at once never overshoot together. The run's
  output is held to that share, everything it used (input and output, its
  sub-agents' included) replaces the reservation afterwards, whether or not
  it succeeded, and once the budget is spent a member claims no further task
  and exits.

A member answers its runs' approval prompts itself, its sub-agents'
included: it approves any call except one to a tool it is denied.

A member's `model` and `instructions` apply to every task it runs. The
limits live in the team config, so they hold for every member however it was
started, including one started with `spawn_teammate`; `sven team create
--token-budget` sets the budget of a team built that way.

List available team definitions in the current project:

```sh
sven team definitions
```

---

## Prompting the lead agent effectively

The lead agent is your interactive sven session.  It has access to all team
tools plus the full standard toolset.

### Be explicit about the outcome

Vague instructions lead the agent to interpret liberally.  Be specific:

| Vague | Specific |
|---|---|
| "Do the refactor" | "Refactor src/auth/ to replace all JWT usage with PASETO.  Write the new token structure in src/auth/token.rs." |
| "Test the thing" | "Run cargo test --workspace.  List every failing test and its error.  Do not fix anything." |
| "Check the code" | "Review src/api/ for missing error handling.  Write a markdown report listing each function with its issue." |

### Tell the agent what to do with the results

If you want a summary, ask for one:

```
After all teammates finish, read their completion summaries and write a
consolidated 'release-notes.md'. Do not summarise the tool calls - only
the actual changes made.
```

If you want the agent to stop and wait for your next instruction:

```
After spawning the teammates and creating the tasks, stop and wait.
Do not poll or summarise until I ask.
```

### Limit the scope when in doubt

Start with a single task and one teammate to verify the workflow before
scaling up:

```
Spawn one explorer teammate.  Ask it to list every function in src/auth/
that takes a token parameter.  Report back when done.
```

Once that works, add more teammates and tasks.

---

## Prompt examples

These are prompts you type into an interactive sven session (or pass on the
command line as `sven "..."`).

```sh
# Create a team and start a multi-step workflow in one shot
sven "
Create a team called 'audit' with goal 'Security audit of the authentication module'.
Spawn two teammates:
  - explorer named 'code-reader' to read src/auth/ and list every place that
    handles passwords or tokens.
  - reviewer named 'sec-reviewer' to review those findings and rate each one
    low/medium/high risk.
Create a 'Read auth module' task for code-reader and a 'Review findings' task
for sec-reviewer that depends on the first.
Monitor with list_tasks until both are complete, then write a summary to
security-report.md.
"

# Check team status on a running team
sven "List the current team status and show all tasks with their statuses."

# Shutdown and clean up a team
sven "Shut down all teammates in team 'audit' and clean up the team."

# Assign a specific task to a named teammate
sven "
Find the task titled 'Review findings' in the task list and assign it to
sec-reviewer using assign_task."
```

---

## What happens inside a teammate

When a teammate starts, it receives:

- Its team name and role from command-line arguments
- A system prompt that tells it to claim tasks from the shared list, work on
  them, and mark them complete with a summary
- The same standard toolset as the lead agent (file read/write, terminal,
  search, web, GDB, etc.)

A teammate's loop typically looks like this:

1. Call `claim_task` to pick up the next available task assigned to it
2. Read any relevant files, run any necessary commands
3. Complete the work
4. Call `complete_task` with a summary of what was done
5. Poll for the next available task, or exit if none remain and the team lead
   has marked it for shutdown

The lead can observe this in real time by calling `list_tasks` and `list_team`.

---

## Recursion and safety

sven enforces hard limits to prevent runaway agent chains:

- **Subprocess depth** - a sub-agent started by the `task` tool cannot itself
  start further sub-agents.
- **SpawnTeammateTool** - only the team lead can spawn teammates.  A teammate
  cannot spawn sub-teams.

A `task` sub-agent never holds more than the session that started it:

- It runs in an ACP mode whose own policy - what its tools do without asking
  included - stays within what the parent may do in every state: an `agent`
  session may start any child, a `research`, `plan` or `chat` session only
  `research`/`plan` children, and an SDLC session none. If the child will not
  switch to that mode, it is stopped rather than left in its default mode.
- A permission request it sends is allowed outright only when the parent
  would run that call itself without asking anyone: the tool's name says
  what it does, the parent's policy allows that without approval (writing
  files in `agent` mode, say), and the parent's host does not ask about
  every call. Anything else - a shell command, an MCP tool, any request under
  an IDE over ACP - is put to the parent's approver: the session's own
  approval gate, as the same prompt (tool and command or path) its own shell
  commands get, or the IDE. A request is refused only when there is neither.
- It never runs a tool the parent session has disabled (`tools.disabled`,
  passed on as `--disable-tool`), takes at most the parent's
  `agent.max_tool_rounds` tool rounds a turn, and writes at most the parent's
  configured output cap per response, never more than its own model allows.
- It is stopped after `agent.child_run_timeout_secs` (default one hour) of
  wall-clock time, however busy, and after 10 minutes without any output -
  a pending permission request pauses that count, and the child waits for
  the answer until its deadline. Exiting before its turn finished, or at its
  deadline, is reported as a failure.
- A request of its still waiting when it stops is taken off the parent's
  screen.
- The tokens it used are reported with its turn and charged where the
  parent's are (a team member's budget, for one).

These limits are enforced at the system level, not by the model.  No prompt
can override them.

---

## Monitoring without a running session

The task store and team config are plain files on disk.  You can inspect them
at any time without a running session:

```sh
# See all tasks for team 'release-prep'
cat ~/.config/sven/teams/release-prep/tasks.json | python3 -m json.tool

# See all registered team members
cat ~/.config/sven/teams/release-prep/config.json | python3 -m json.tool
```

Team definitions (YAML files) live in `.sven/teams/` inside your project.

---

## Troubleshooting teams

### Teammate appears in logs but not in `list_team`

The teammate process has started but has not yet registered itself in the
shared team config.  Wait a few seconds and call `list_team` again.

### Task stuck in `in_progress` with no activity

The teammate that claimed the task may have crashed or been killed.  Check
with `list_team` to see if it is still marked Active.  You can update the task
description to re-clarify requirements, then wait to see if it recovers - or
shut down the stuck teammate with `shutdown_teammate` and respawn it.

### `cleanup_team` refuses to run

By default, `cleanup_team` refuses if any teammates are still marked Active,
to prevent data loss.  Shut down all teammates first with `shutdown_teammate`,
then call `cleanup_team`.  If a teammate process has already been killed by the
OS, pass `force: true` to force cleanup anyway.

### Team directory already exists after a crash

If a previous run did not clean up properly, the team directory may already
exist on disk.  Either delete it manually:

```sh
rm -rf ~/.config/sven/teams/<team-name>
```

Or call `cleanup_team` with `force: true` from the lead agent.

---

## Reference - team and task tools

These tools are available to the interactive (lead) agent.

### Team lifecycle

| Tool | What it does |
|---|---|
| `create_team` | Initialise a new team.  You become the lead.  Creates the shared task store. |
| `list_team` | Show all members with their role, status, and current task. |
| `spawn_teammate` | Start a new sven process as a teammate.  Only the lead can do this. |
| `shutdown_teammate` | Signal a teammate to finish its current task and exit. |
| `register_teammate` | Register an already-running sven process as a team member (for manually started teammates). |
| `merge_teammate_branch` | Merge a teammate's Git worktree branch into the current branch. |
| `cleanup_team` | Remove the team directory.  Requires all teammates to be shut down first. |

### Task management

| Tool | What it does |
|---|---|
| `create_task` | Add a task to the shared board. |
| `list_tasks` | Show all tasks with status, assignee, and summaries. |
| `assign_task` | Direct a task to a specific teammate. |
| `claim_task` | Mark a task as in-progress (called by teammates, not usually the lead). |
| `complete_task` | Mark a task done and record a summary of what was accomplished. |
| `update_task` | Edit a task description after creation. |
