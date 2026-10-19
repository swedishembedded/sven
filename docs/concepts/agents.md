# Agents and modes

## Agent modes in practice

Modes control both the state machine that drives the session and the tools the
agent is allowed to use. Choosing the right mode prevents unintended changes and
gives you the right level of formality for the task.

### `chat` - conversational coding assistant (default)

`chat` (along with `agent` and `reactive`) is powered by the
`ReactiveAgentMachine`. It runs a streaming, native-tool-calling agent loop: the
model reasons, calls tools (read, search, edit, shell, …) that are executed and
fed back, and streams its answer. For quick questions, code review, exploratory
analysis, and edits, this is the right mode.

```sh
sven "What does the authentication module do?"
sven "Explain the race condition in this code."
```

The agentic model↔tool loop that powers a single turn is the same reusable
`Deliberator` engine described in
[Deliberation Engine](deliberation.md).

### `sdlc` - software development lifecycle

`sdlc` is powered by the `SdlcMachine` - a deliberation-driven HSM that encodes
the engineering lifecycle as a sequence of phases, each of which runs its own
scoped LLM↔tool deliberation on an append-only conversation thread and returns a
structured decision that drives the next transition:

```
Idle → Intake → Discovery → Planning → Execution → Verification → Delivery
                   │                                                  │
                   └──────────────── Recovery ◀──────────────────────┘
```

**Intake guard**: `Intake` classifies your request before any work happens.
Chit-chat or under-specified requests keep the machine in `Intake` and ask
clarifying questions; only a confirmed, actionable scope advances to `Discovery`.

**Human gates**: a phase that needs your input asks you in the TUI. Under
`--approval manual` it also pauses for your approval at scope, plan, and
delivery; by default those are approved. In CI / headless mode
(`RuntimeRunner`) nobody is asked: a question is answered at once saying no
user is available, so a run completes unattended.

**Parallel execution**: `Execution` decomposes the plan and fans out one
concurrent child kernel per task, then merges the child summaries back into the
parent thread. See [Parallel Submachine Fan-out](../internals/parallel-submachines.md).

**Recovery**: when a deliberation reports failure, the hierarchy routes to a
`Recovery` phase that diagnoses and decides whether to retry or abort.

For the full design - the decision envelope, per-state tools and models, and the
append-only / cache-safe thread invariant - see the
[Deliberation Engine](deliberation.md) reference.

Use `sdlc` when you want sven to implement a feature end-to-end with full
traceability, or - with `--approval manual` - when you want to approve scope,
plan and delivery yourself:

```sh
sven --mode sdlc "Add rate limiting to the API."
SVEN_MODE=sdlc sven --file plan.md
```

### `research` - safe exploration

The agent can only read. It can run commands like `ls`, `cat`, `grep`, and
`find`, but cannot write to any file.

```sh
sven --mode research "What does the authentication module do?"
```

### `plan` - structured proposals

The agent reads freely and produces a written plan but does not write any
files. Use this before an `agent` or `sdlc` run to review what will happen.

```sh
sven --mode plan "Design a rate-limiting layer for the API."
```

### `agent` - full access

The agent can read, write, delete files and run any command. Use for
general-purpose agentic tasks where the formal SDLC structure is not needed.

```sh
sven "Implement the rate-limiting layer described in the plan."
```

### Cycling modes live

Press `F4` inside the TUI to cycle through `research → plan → agent → research`.
For `chat` and `sdlc`, use the `/mode` command or restart with `--mode`.

### Approving tool calls

By default (`--approval auto`) every call the mode allows runs without a
prompt. Start the TUI with `sven --approval manual` to approve each call that
is not read-only yourself: it opens as a question modal showing the tool and
its command, path or arguments, with `yes` and `no` options. A `task`
sub-agent's calls and an SDLC task child's calls come to the same modal. Each
call is asked about on its own - approving one `shell` command does not
approve the next.

| Key | Action |
|-----|--------|
| `↑` / `↓`, `1`-`9` | Choose `yes` or `no` |
| `Enter` | Answer - `yes` runs the call, anything else refuses it |
| `Esc` | Dismiss - the call is refused |

A refused call is answered to the model with the refusal, so it can take
another route. An SDLC decision that needs approval (`need_approval`) is put
to you the same way under manual approval, and approved automatically under
auto.
