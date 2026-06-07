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
| `chat` | Conversational coding assistant. A streaming, native-tool-calling agent (the same engine as `agent`/`reactive`) for questions, analysis, code review, and edits. **Default.** |
| `sdlc` | Full software-development lifecycle. Formally structured: Intake → Discovery → Planning → Execution → Verification → Delivery, with a Recovery path. Each phase runs a scoped LLM↔tool deliberation and pauses for human approval at scope, plan, and delivery. |
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

Sven's agent runtime is built on a formally-specified **Hierarchical State
Machine (HSM)** kernel, not a free-running LLM loop. The distinction matters:
the kernel owns control flow deterministically and auditably, while the LLM does
its reasoning and tool use *inside* a well-defined step.

```
  Your message
      │
      ▼  Event
  ┌──────────────────────────────────┐
  │  HSM Kernel  (pure, no I/O)      │
  │  current state decides what next │
  └──────────────────────────────────┘
      │  Effects (data, not calls)
      ▼
  ┌──────────────────────────────────┐
  │  Executors  (the only I/O layer) │
  │  LLM-loop · Tool · User · Timer  │
  └──────────────────────────────────┘
      │  one completion event → queue
      ▼
  (back to HSM kernel)
```

**What the kernel does.** A machine reacts to a typed `Event`, updates its
state, and *returns* `Effect`s describing the side effects it wants. The kernel
itself never performs I/O. It validates every effect against a permission policy,
hands each to an executor, and records every transition to an append-only audit
log so any session can be replayed exactly - useful for debugging and for tests
that assert on whole session traces without a real LLM or network.

**What the LLM does.** This depends on the mode. In the default chat/agent modes
and in each SDLC phase, an executor runs a real model↔tool agentic loop: the
model streams reasoning and makes **native tool calls** (read, search, edit,
shell, …) that are executed and fed back, exactly like a modern coding agent.
The kernel stays the authority over *transitions* - the loop runs inside a single
`CallLlm` effect and reports back exactly one completion event when the step
settles. In `sdlc` mode that completion is a structured **decision** whose status
(proceed / need user input / need approval / re-deliberate / failed) drives the
next transition.

**What executors do.** Executors are the only code that performs I/O. They stream
live progress (text, thinking, tool starts/finishes, token usage) outward for
the UI to render, then post one result event back into the kernel queue. A
`UserExecutor` surfaces a question or approval prompt and waits for your
decision.

In headless / CI mode the same kernel runs with every human gate auto-approved,
writing final output to stdout. For the full technical design see
**[HSM Architecture](technical/hsm-architecture.md)**, the **[Deliberation
Engine](technical/deliberation-engine.md)**, and **[Parallel Submachine
Fan-out](technical/parallel-submachines.md)**.

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
- **[Deliberation Engine](technical/deliberation-engine.md)** - how each SDLC phase runs a scoped LLM↔tool loop and returns a decision that drives the machine
- **[Parallel Submachine Fan-out](technical/parallel-submachines.md)** - how sven runs plan tasks concurrently in isolated child kernels
