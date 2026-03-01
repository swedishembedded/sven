# Introduction

## What is sven?

sven is an AI coding agent that lives in your terminal. You give it a task — in
plain English — and it works autonomously: reading files, running commands,
writing code, and reporting back as it goes. When the task is done, sven stops
and hands control back to you.

It works in two ways:

- **Interactive** — a full-screen terminal interface where you chat with the
  agent, watch it work in real time, and steer it mid-task.
- **Headless** — reads instructions from a file or standard input, writes clean
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
- **Autonomous embedded hardware debugging** via native GDB integration — sven
  is the first AI agent that can start a GDB server, connect to a physical
  device, set breakpoints, inspect memory and variables, and report findings
  entirely on its own
- **Agent-to-agent task routing** — multiple sven instances can find each other
  on a local network (or across the internet via a relay), delegate subtasks to
  each other, and assemble the results — no human in the loop required

---

## Agent modes

Every sven session runs in one of three modes. The mode controls what the agent
is allowed to do, so you can give it exactly the access the task needs.

| Mode | What the agent can do |
|------|----------------------|
| `research` | Read files, run read-only commands (grep, ls, cat). No writes. |
| `plan` | Same as research, plus it can produce structured plans. No file writes. |
| `agent` | Full access: read and write files, run any command, use all tools. |

Use `research` when you want the agent to explore and report without touching
anything. Use `plan` when you want a structured proposal you can review before
acting. Use `agent` when you are ready to let it work.

The mode can be set on the command line with `--mode` and cycled live inside
the TUI with `F4`.

---

## How sven works

When you send a message, sven forwards it to a large language model (OpenAI
GPT-4o by default, or Anthropic Claude). The model decides what to do and can
ask sven to execute tools — reading files, running commands, searching the
codebase. The results go back to the model, which continues reasoning until the
task is complete or it needs to ask you something.

All of this happens in the background. In the TUI you see the conversation and
tool calls stream in as they happen. In headless mode the final text is written
to standard output.

---

## Where to go next

- **[Installation](01-installation.md)** — get sven onto your machine
- **[Quick Start](02-quickstart.md)** — run your first session in five minutes
- **[User Guide](03-user-guide.md)** — TUI navigation, features, and tips
- **[CI and Pipelines](04-ci-pipeline.md)** — use sven in scripts and CI
- **[Configuration](05-configuration.md)** — customise model, tools, and appearance
- **[Examples](06-examples.md)** — real-world use cases
- **[Troubleshooting](07-troubleshooting.md)** — common issues and fixes
- **[Remote Gateway](08-gateway.md)** — expose agents over HTTPS/P2P, pair devices, route tasks between agents
