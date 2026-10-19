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
| `sdlc` | Full software-development lifecycle. Formally structured: Intake → Discovery → Planning → Execution → Verification → Delivery, with a Recovery path. Each phase runs a scoped LLM↔tool deliberation; with `--approval manual` it pauses for your approval at scope, plan, and delivery. |
| `research` | Read files and run read-only commands. No writes. |
| `plan` | Reads freely, produces structured plans, no file writes. |
| `agent` | Full read/write access. Use for general-purpose agentic tasks. |

Set with `--mode <name>` on the command line, or via `SVEN_MODE=<name>` in the
environment. Cycle the legacy read/plan/agent trio inside the TUI with `F4`.

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
`UserExecutor` surfaces a question - and, under `--approval manual`, an
approval prompt - and waits for your decision.

By default every call the mode allows runs without a prompt. In headless / CI
mode nobody is there to answer, so the same kernel answers every gate at once
- a question gets "No user is available; proceed on your best judgement and
state the assumption" - and writes final output to stdout. For the full technical design see
**[HSM Architecture](internals/hsm-architecture.md)**, the **[Deliberation
Engine](concepts/deliberation.md)**, and **[Parallel Submachine
Fan-out](internals/parallel-submachines.md)**.

---

## Where to go next

- **[Installation](getting-started/installation.md)** and **[Quick start](getting-started/quickstart.md)** - get sven running and complete a first session
- **[Interactive use](guides/interactive.md)** - the terminal interface, conversations and slash commands
- **[Headless and CI](guides/headless.md)** and **[Workflow files](guides/workflows.md)** - run sven in scripts and pipelines
- **[Teams](guides/teams.md)** and **[Harnesses](guides/harnesses.md)** - orchestrate several agents, or write an agent as a YAML program
- **[Agents and modes](concepts/agents.md)** - what each mode may do and how approvals apply
- **[Configuration](reference/configuration.md)**, **[Providers](reference/providers.md)** and **[Tools](reference/tools.md)** - the reference
- **[SDK](reference/sdk.md)** - embed the agent kernel in your own application
- **[Troubleshooting](guides/troubleshooting.md)** - common problems and fixes
