# Introduction

## What is sven?

sven is an AI coding agent that lives in your terminal. You give it a task - in
plain English - and it works autonomously: reading files, running commands,
writing code, and reporting back as it goes. When the task is done, sven stops
and hands control back to you.

It works in two ways:

- **Interactive** - a full-screen terminal interface where you chat with the
  agent, watch it work in real time, and steer it mid-task.
- **Headless** - reads instructions from a file or standard input, writes clean
  text to standard output, and exits. Fits naturally into shell scripts, CI
  pipelines, and automated workflows.

Both modes use the same agent core, so a workflow you develop interactively can
be run unattended without any changes.

---

## What can sven do?

sven can perform any task that involves reading and writing files, searching
code, and running shell commands. Common uses include:

- Analysing an unfamiliar codebase and producing a summary
- Implementing a feature based on a description
- Refactoring code to meet a style guide
- Writing and running tests
- Reviewing a pull request diff and suggesting improvements
- Automating multi-step CI tasks that normally require manual intervention
- **Autonomous embedded hardware debugging** via native GDB integration - sven
  is the first AI agent that can start a GDB server, connect to a physical
  device, set breakpoints, inspect memory and variables, and report findings
  entirely on its own
- **Agent-to-agent task routing** - multiple sven instances can find each other
  on a local network (or across the internet via a relay), delegate subtasks to
  each other, and assemble the results - no human in the loop required

---

## Agent modes

Every sven session runs in a mode that controls the machine driving it and what
tools it is allowed to use.

| Mode | What the agent can do |
|------|----------------------|
| `chat` | Conversational assistant. Handles questions, analysis, and code review. Automatically hands off engineering tasks to the `sdlc` machine when needed. **Default.** |
| `sdlc` | Full software-development lifecycle. Formally structured: Intake → Discovery → Planning → Execution (patch → build → test → lint) → Verification → Delivery. Requires human approval before applying changes. |
| `research` | Read files and run read-only commands. No writes. |
| `plan` | Reads freely, produces structured plans, no file writes. |
| `agent` | Full read/write access. Use for general-purpose agentic tasks. |

Set with `--mode <name>` on the command line, or via `SVEN_MODE=<name>` in the
environment. Cycle the legacy read/plan/agent trio inside the TUI with `F4`.

---

## Running as a node - talking to other agents

`sven` by itself is a local session: one agent, one conversation.

`sven node start` is the peer-enabled form: the same agent runs a P2P stack
alongside its normal session, discovers other sven nodes on the network (or via
a relay), and gains a set of collaboration tools - `send_message`,
`wait_for_message`, `search_conversation`, `post_to_room`, and more.

```sh
# Start the node (runs until Ctrl-C)
sven node start

# From another terminal - ask the node's agent to talk to a peer
sven node exec "Ask backend-agent to explain the auth module, wait for its reply."

# Or open an interactive TUI session directly with a remote peer
sven peer chat backend-agent
```

See [Sven Node](08-node.md) and
[Agent Collaboration](09-collaboration.md) for the full setup guide.

---

## How sven works

Sven's agent loop is a formally-specified **Hierarchical State Machine (HSM)**,
not a free-running LLM loop. The distinction matters: control flow is
deterministic and auditable; the LLM is a *reasoning service* that proposes
typed actions but never executes anything directly.

```
  Your message
      │
      ▼  Event
  ┌──────────────────────────────────┐
  │  HSM Kernel  (pure, no I/O)     │
  │  current state + guards decide  │
  └──────────────────────────────────┘
      │  Effects (data, not calls)
      ▼
  ┌──────────────────────────────────┐
  │  Executors  (the only I/O layer) │
  │  LLM · Tool · User · Timer      │
  └──────────────────────────────────┘
      │  Result events → queue
      ▼
  (back to HSM kernel)
```

**What the LLM does.** When the machine needs reasoning (e.g. understanding
your intent, proposing a code patch, classifying a build failure), it emits a
`CallLlm` effect with a typed `LlmRequest`. The LLM returns structured JSON
parsed into a typed response. It never names a tool to invoke.

**What the machine does.** Based on the LLM response and the current state,
the machine transitions and emits further effects - perhaps `CallTool` to run
the proposed patch through the build system, `AskUser` to ask a clarifying
question, or `RequestHumanApproval` before applying any externally-visible
change.

**What executors do.** Executors are the only code that performs I/O. A
`ToolExecutor` runs the requested tool and posts `ToolSucceeded` or
`ToolFailed` back to the kernel queue. A `UserExecutor` surfaces an approval
prompt in the TUI and waits for your decision.

Every state transition is written to an append-only JSONL audit log. Any
session can be replayed exactly - useful for debugging and for writing tests
that assert on full session traces without needing a real LLM or network.

When the model requests multiple tools in one turn, the `ToolExecutor` runs
them in parallel and posts results back individually. In headless / CI mode,
`RuntimeRunner` drives the same kernel with `auto_approve: true` and writes
final output to stdout.

---

## Where to go next

- **[Installation](01-installation.md)** - get sven onto your machine
- **[Quick Start](02-quickstart.md)** - run your first session in five minutes
- **[User Guide](03-user-guide.md)** - TUI navigation, features, and tips
- **[CI and Pipelines](04-ci-pipeline.md)** - use sven in scripts and CI
- **[Configuration](05-configuration.md)** - customise model, tools, and appearance
- **[Examples](06-examples.md)** - real-world use cases
- **[Troubleshooting](07-troubleshooting.md)** - common issues and fixes
- **[Sven Node](08-node.md)** - expose agents over HTTPS/P2P, pair devices, route tasks between agents
- **[Agent Collaboration](09-collaboration.md)** - persistent peer conversations, rooms, and the `sven peer chat` command
- **[Teams and Tasks](11-teams-and-tasks.md)** - form a team of agents, break work into tasks, and orchestrate parallel workstreams
- **[HSM Architecture](technical/hsm-architecture.md)** - full technical reference for the hierarchical state machine kernel
