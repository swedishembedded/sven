# Teams and Tasks

When a task is too large for one agent to handle cleanly — or when you want
different parts of a problem to run in parallel — sven supports **teams**: a
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
write one good prompt.  The agent handles everything else — creating the team,
spawning teammates, creating tasks, assigning them, tracking completion, and
reporting results back to you.

### The simplest approach: one prompt

```
sven node exec "Refactor the auth module in src/auth/ to use PASETO instead of JWT.

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
- Call `spawn_teammate` twice — once for the explorer, once for the implementer
- Call `create_task` for each piece of work
- Monitor with `list_tasks` until everything is completed
- Report back to you

---

## Interactive walkthrough

The following prompts show how to drive a team step by step, as you would in
the `sven node start` web UI or via `sven node exec`.

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
Check the team status — who is working on what and how far along are the tasks?
```

The agent calls `list_team` and `list_tasks` and reports back:

```
Team 'release-prep' — 3 members | tasks: pending=0, in_progress=2, completed=0, failed=0

● changelog-writer [Explorer] — working on: "Write CHANGELOG entry"
● test-runner      [Tester]   — working on: "Run test suite"
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
the LLM about what the agent should focus on — it is injected into the system
prompt.

| Role | Typical use |
|---|---|
| `explorer` | Investigate a codebase area, gather information, write notes |
| `implementer` | Write or change code based on a specification |
| `reviewer` | Read code and produce a review report, suggest improvements |
| `tester` | Run tests, analyse failures, suggest fixes |
| `teammate` | General purpose — no specific focus |

You can also use any custom string: `role: "documentation-writer"` or
`role: "security-auditor"` work fine.

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
      error messages. Do not attempt fixes — only report.

  - role: reviewer
    name: pr-reviewer
    instructions: |
      Read the diff since the last tag. Write a review covering correctness,
      edge cases, and any potential regressions.
```

Start it:

```sh
sven team start --file .sven/teams/release.yaml
```

This creates the team and spawns all members automatically.  No prompting
needed.

List available team definitions in the current project:

```sh
sven team definitions
```

---

## Prompting the lead agent effectively

The lead agent is the node's interactive agent — the one you talk to via the
web UI or `sven node exec`.  It has access to all team tools plus the full
standard toolset.

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
consolidated 'release-notes.md'. Do not summarise the tool calls — only
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

## `sven node exec` examples

These commands can be run from any terminal where the node is reachable.  Set
`SVEN_NODE_TOKEN` first (printed on first startup; rotate with
`sven node regenerate-token`).

```sh
export SVEN_NODE_TOKEN=<your-token>

# Create a team and start a multi-step workflow in one shot
sven node exec "
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
sven node exec "List the current team status and show all tasks with their statuses."

# Shutdown and clean up a team
sven node exec "Shut down all teammates in team 'audit' and clean up the team."

# Ask a teammate to take over a specific task
sven node exec "
Find the task titled 'Review findings' in the task list and assign it to
sec-reviewer. Ask sec-reviewer via delegate_task to claim and complete it."
```

---

## What happens inside a teammate

When a teammate starts, it receives:

- Its team name, role, and lead's peer ID from command-line arguments
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

- **Subprocess depth** — a teammate spawned by `TaskTool` (the local subprocess
  spawner) cannot itself spawn further sub-agents.  The maximum depth is 3
  levels.
- **P2P hop depth** — tasks delegated over the network cannot chain more than 4
  hops.  A node that receives a delegated task does not get team tools, so it
  cannot create new teams or spawn new teammates.
- **Cycle detection** — the delegation chain is tracked by peer ID.  If a task
  would loop back to a node already in the chain, it is rejected before the
  model is even invoked.
- **SpawnTeammateTool** — only the team lead can spawn teammates.  A teammate
  cannot spawn sub-teams.

These limits are enforced at the system level, not by the model.  No prompt
can override them.

---

## Monitoring without a running session

The task store and team config are plain files on disk.  You can inspect them
at any time without a running node:

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

The teammate process has started but has not yet connected to the P2P mesh and
registered itself.  Wait a few seconds and call `list_team` again.  If it
never appears, check that both nodes have each other in `swarm.peers` and that
the swarm port is reachable.

### Task stuck in `in_progress` with no activity

The teammate that claimed the task may have crashed or been killed.  Check
with `list_team` to see if it is still marked Active.  You can update the task
description to re-clarify requirements, then wait to see if it recovers — or
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

## Reference — team and task tools

These tools are available to the node's interactive (lead) agent.

### Team lifecycle

| Tool | What it does |
|---|---|
| `create_team` | Initialise a new team.  You become the lead.  Creates the shared task store. |
| `list_team` | Show all members with their role, status, and current task. |
| `spawn_teammate` | Start a new sven process as a teammate.  Only the lead can do this. |
| `shutdown_teammate` | Signal a teammate to finish its current task and exit. |
| `register_teammate` | Register an already-running peer as a team member (for manually started nodes). |
| `merge_teammate_branch` | Merge a teammate's Git worktree branch into the current branch. |
| `broadcast_abort` | Signal all teammates to abort immediately. |
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
