# Introduction

## What is sven?

sven is an AI coding agent for the terminal and for CI, and the agent execution
framework it is built on. You give it a task in plain English; it reads files,
runs commands, edits code and reports back, then stops and hands control back to
you.

It runs in two ways:

- **Interactive**: a full-screen terminal interface (`sven`) where you chat with
  the agent, watch it work, and steer it mid-task.
- **Headless**: `sven "task"`, a piped task, or `--headless` run without the
  interface, write text to standard output, and exit. This fits shell scripts, CI
  pipelines and workflow files.

Both use the same agent core, so a task you develop interactively runs unattended
unchanged.

---

## What can sven do?

- Analyse an unfamiliar codebase and summarise it
- Implement, refactor and test code from a description
- Review a diff and suggest changes
- Run multi-step workflow files in CI
- Debug embedded targets through GDB tools: start a GDB server, connect to a
  device, set breakpoints and inspect memory and variables
- Split work across several agents: sub-agents inside a session (`task`) and
  teams of sven processes sharing a task board
- Serve other tools as an MCP server or an ACP agent for IDEs
- Be embedded in your own program through the SDK

---

## Agent modes

Every session runs in a mode, chosen with `--mode` (`-m`), `/mode` or `F4`. The
mode decides which state machine drives the session and what the kernel lets it
do.

| Mode | What the agent can do |
|------|----------------------|
| `agent` | Read and write files, run shell commands, use the network. **Default.** |
| `chat` | The same machine and permissions as `agent`, prompted as a conversational assistant; its sub-agents are read-only. |
| `research` | Read-only tools: no shell, no file writes. |
| `plan` | Reads and runs commands, produces a plan; the file-write tools are refused. |
| `sdlc` | A phased lifecycle: Intake, Discovery, Planning, Execution, Verification, Delivery, with a Recovery path. Writes and shell are available only in the phases that need them. |

Approval is a separate switch. By default (`--approval auto`) every call a mode
permits runs without asking; `--approval manual` asks you before each call that
is not read-only. [Agents and modes](concepts/agents.md) has the exact rules.

---

## How sven works

Sven's runtime is a formally specified **hierarchical state machine (HSM)**
kernel, not a free-running model loop. The kernel owns control flow
deterministically and auditably; the model reasons and calls tools inside a
well-defined step.

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

A machine reacts to a typed `Event`, updates its state, and returns `Effect`s
describing the side effects it wants. The kernel never performs I/O: it checks
every effect against the session's permission policy, hands it to an executor, and
records every transition to an append-only audit log, so a session can be replayed
exactly and tests can assert on whole traces without a model or network.

Executors are the only code that performs I/O. In `agent`, `chat` and each `sdlc`
phase, an executor runs a model and tool loop: the model streams text and makes
native tool calls that are executed and fed back. The loop runs inside one
`CallLlm` effect and reports a single completion event when the step settles; in
`sdlc` that completion is a structured decision (proceed, need user input, need
approval, re-deliberate, failed) which drives the next transition.

When a question or approval needs a person, the user executor asks in the TUI. With
nobody there (headless), a question is answered at once with "no user is
available" and the run continues; manual approval is refused at start-up.

For the design, see the [HSM architecture](internals/hsm-architecture.md), the
[Deliberation engine](concepts/deliberation.md) and
[Parallel submachine fan-out](internals/parallel-submachines.md).

---

## Where to go next

- **[Installation](getting-started/installation.md)** and **[Quick start](getting-started/quickstart.md)**: get sven running and complete a first session
- **[Interactive use](guides/interactive.md)**: the terminal interface, sessions and slash commands
- **[Headless and CI](guides/headless.md)** and **[Workflow files](guides/workflows.md)**: run sven in scripts and pipelines
- **[Teams](guides/teams.md)** and **[Harnesses](guides/harnesses.md)**: orchestrate several agents, or write an agent as a YAML program
- **[Agents and modes](concepts/agents.md)**: what each mode may do and how approvals apply
- **[Configuration](reference/configuration.md)**, **[Providers](reference/providers.md)** and **[Tools](reference/tools.md)**: the reference
- **[SDK](reference/sdk.md)**: embed the agent kernel in your own application
- **[Troubleshooting](guides/troubleshooting.md)**: common problems and fixes
