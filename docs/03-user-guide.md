# User Guide

## The TUI in depth

### Layout

The TUI has four visual regions:

```
┌─────────────────────────────────────────────────┐
│ gpt-4o  agent  ctx:18%  ⠿ run_terminal_command  │  status bar
├─────────────────────────────────────────────────┤
│                                                  │
│   chat pane                                      │
│   (scrollable conversation history)              │
│                                                  │
├─────────────────────────────────────────────────┤
│ > type here and press Enter                      │  input box
└─────────────────────────────────────────────────┘
```

**Status bar** — always visible at the top. Shows:
- Model name (e.g. `gpt-4o`)
- Current agent mode (`research`, `plan`, or `agent`)
- Context usage as a percentage (`ctx:18%`)
- A spinner and the name of any tool currently running

**Chat pane** — the conversation history. User messages, agent responses, and
collapsed tool calls are all shown here. Scrolls independently of the input box.

**Input box** — a multi-line text field. Press `Enter` to send, `Shift+Enter`
to insert a newline.

---

### Focus and pane switching

The TUI has two focusable panes: the chat pane and the input box. Focus starts
on the input box.

Switch focus with the `Ctrl+W` chord:

| Sequence | Effect |
|----------|--------|
| `Ctrl+W` then `K` or `↑` | Focus the chat pane |
| `Ctrl+W` then `J` or `↓` | Focus the input box |

When the chat pane has focus, navigation keys work as described below. When the
input box has focus, all printable characters go to the text field.

---

### Scrolling and navigation (chat pane)

First switch focus to the chat pane with `Ctrl+W K`.

| Key | Action |
|-----|--------|
| `j` / `↓` | Scroll down one line |
| `k` / `↑` | Scroll up one line |
| `J` | Scroll down one line (shift variant) |
| `K` | Scroll up one line (shift variant) |
| `Ctrl+D` | Scroll down half a page |
| `Ctrl+U` | Scroll up half a page |
| `g` | Jump to the very top |
| `G` | Jump to the very bottom |

---

### Search

Press `/` while the chat pane has focus to open the search bar at the bottom
of the screen. Type to filter the conversation in real time.

| Key | Action |
|-----|--------|
| `/` | Open search |
| `n` | Jump to next match |
| `N` | Jump to previous match |
| `Esc` or `Enter` | Close search and stay on the current match |

---

### Editing a past message

If you want to correct or rephrase a message you already sent, navigate to it
in the chat pane and press `e`. The message text appears in the input box for
editing. When you are happy with the change, press `Enter` to re-submit it as
if it were a new message. Press `Esc` to cancel and restore the original.

---

### Full-screen pager

Press `Ctrl+T` to open the full-screen pager. This expands the chat history to
fill the whole terminal, which is useful for reading long responses or code
blocks without the input box taking up space. Press `Esc` or `q` to close the
pager.

---

### Help overlay

Press `F1` to toggle the in-app help overlay, which lists all key bindings.

---

### Neovim integration

By default, sven embeds a headless Neovim instance and uses it as the chat
buffer. This gives the chat pane full Neovim editing capabilities:

- Navigate and scroll the chat with all standard Neovim motions
- Use `:q` or `:qa` to quit sven
- Press `Ctrl+Enter` from the chat pane to submit the buffer content as a
  message

Sven defaults to the plain ratatui view.  To enable the embedded Neovim chat
pane instead, pass `--nvim`:

```sh
sven --nvim
```

In the default ratatui mode, tool calls and thinking blocks in the history are
collapsed by default to keep the view compact.

---

## Agent modes in practice

Modes control what tools the agent is allowed to use. Choosing the right mode
prevents unintended changes and makes the agent's output more predictable.

### `research` — safe exploration

The agent can only read. It can run commands like `ls`, `cat`, `grep`, and
`find`, but cannot write to any file. Use this when you want to explore a
codebase without any risk of modification.

```sh
sven --mode research "What does the authentication module do?"
```

### `plan` — structured proposals

The agent reads freely and produces a written plan but does not write any
files. The output is typically a list of steps or a design document. Use this
before an `agent` run to review what will happen.

```sh
sven --mode plan "Design a rate-limiting layer for the API."
```

### `agent` — full access

The agent can read, write, delete files and run any command. This is the
default mode. Use it when you want sven to implement something end-to-end.

```sh
sven "Implement the rate-limiting layer described in the plan."
```

### Cycling modes live

Press `F4` inside the TUI to cycle through `research → plan → agent → research`.
The status bar updates immediately to show the new mode. Changes take effect on
the next message you send.

---

## Tools and approvals

sven has a set of built-in tools it can call to complete tasks:

| Tool | What it does |
|------|-------------|
| `run_terminal_command` | Run a shell command |
| `read_file` | Read a file |
| `write` | Write or create a file |
| `edit_file` | Edit part of a file |
| `delete_file` | Delete a file |
| `list_dir` | List directory contents |
| `glob_file_search` | Find files by pattern |
| `grep` | Search file contents |
| `search_codebase` | Semantic search of a codebase |
| `apply_patch` | Apply a unified diff patch |
| `web_fetch` | Fetch a URL |
| `web_search` | Search the web |
| `read_lints` | Read linter diagnostics |
| `todo_write` | Track tasks in the current session |
| `ask_question` | Ask you a clarifying question |
| `switch_mode` | Change the agent mode mid-session |
| `gdb_start_server` | Start a GDB server in the background |
| `gdb_connect` | Connect gdb-multiarch to the running server |
| `gdb_command` | Run a GDB command and return its output |
| `gdb_interrupt` | Interrupt execution (Ctrl+C equivalent) |
| `gdb_stop` | Stop the debugging session and kill the server |

### GDB debugging tools

Sven is the **first AI agent with native GDB integration** for autonomous
embedded hardware debugging. Give it a plain-English task and it handles the
entire debug lifecycle — from starting the server and loading firmware through
setting breakpoints, inspecting state, and cleaning up — without any manual
intervention.

The five GDB tools form a lifecycle:

```
gdb_start_server → gdb_connect → gdb_command / gdb_interrupt → gdb_stop
```

The screenshots below show a real session: the user asks sven to find the
parameters passed to an nRF UART TX function, and the agent works through the
full debug cycle autonomously.

**Task start and target discovery** (ratatui TUI):

![sven GDB session — task start](sven-gdb-1.png)

**Inspecting parameters and final summary** (ratatui TUI):

![sven GDB session — result](sven-gdb-2.png)

**Same session in the embedded Neovim view**:

![sven GDB session — Neovim](sven-gdb-nvim.png)

**Starting a server**

If you do not supply a command, sven searches your project for configuration
hints in this order:

1. `.gdbinit` — looks for `# JLinkGDBServer ...` comments or `target remote` lines
2. `.vscode/launch.json` — reads `debugServerPath`, `debugServerArgs`, and `servertype`
3. `openocd.cfg` — builds an OpenOCD command from the config file
4. `platformio.ini` — reads `debug_server` or `debug_tool`
5. `CMakeLists.txt` / `Cargo.toml` — matches MCU family names (STM32, AT32, NRF, …)

If discovery fails, sven asks you to supply the command explicitly.

**Example session**

```
User: Flash and debug my firmware. The device is an AT32F435RMT7.

Agent calls:
  gdb_start_server {"command": "JLinkGDBServer -device AT32F435RMT7 -if SWD -speed 4000 -port 2331"}
  gdb_connect      {"executable": "build/firmware.elf"}
  gdb_command      {"command": "load"}
  gdb_command      {"command": "break main"}
  gdb_command      {"command": "continue"}
  gdb_command      {"command": "info registers"}
  gdb_stop
```

### Approval policy

Before running a shell command, sven checks it against approval rules:

- **Auto-approved** patterns run without prompting (e.g. `cat *`, `ls *`,
  `grep *`).
- **Denied** patterns are blocked outright (e.g. `rm -rf /*`).
- Everything else is presented for confirmation if the agent requests it.

You can customise these patterns in the configuration file — see
[Configuration](05-configuration.md).

---

## Conversation management

### Starting and continuing conversations

Every TUI session is automatically saved. When you close sven, the conversation
is written to a file in `~/.config/sven/history/`.

To list saved conversations:

```sh
sven chats
```

Output:

```
ID (use with --resume)                          DATE              TURNS  TITLE
-----------------------------------------------------------------------------------------------
3f4a...                                         2025-01-15 10:42  12     Codebase analysis
a1b2...                                         2025-01-14 09:11  5      Rate limiter design
```

To resume a session, pass its ID (or a unique prefix) to `--resume`:

```sh
sven --resume 3f4a
```

If you omit the ID entirely, sven opens an interactive fuzzy-finder (requires
`fzf`) so you can pick the session visually:

```sh
sven --resume
```

### Conversation files

For longer-running work, a conversation file gives you a plain-text record that
you can edit directly. See [Quick Start](02-quickstart.md) for an introduction,
and [CI and Pipelines](04-ci-pipeline.md) for the full file format.

---

## Context and compaction

Every message you send, every tool call, and every response is stored in the
conversation context. Language models have a finite context window, and when
the conversation grows long enough, older messages must be summarised to make
room for new ones.

sven tracks context usage and shows it in the status bar (`ctx:X%`). When usage
reaches the configured threshold (85% by default), sven automatically compacts
the oldest part of the conversation into a short summary before sending the
next message. This happens transparently in the background.

You will not lose any information that sven has already used; compaction only
affects how much raw history the model can see at once.

---

## Interrupting the agent

If the agent is in the middle of a long task and you want to stop it, press
`Ctrl+C` from the input box. The current tool call is cancelled and sven
returns to idle, ready for your next message.

---

## The `/quit` command

To exit sven from the input box, type `/quit` and press `Enter`. In the Neovim
buffer, use `:q` or `:qa`.

---

## Skills

Skills are instruction packages that teach sven how to handle a specific type
of task.  Each skill lives in its own directory alongside any helper scripts or
reference files it needs.  When you invoke a skill, sven loads its instructions
and follows them for your task.

Skills are discovered automatically from your project and home directory on
startup.  You do not need to configure anything; drop a skill directory in the
right place and it appears immediately.

---

### Invoking a skill with a slash command

Every discovered skill is registered as a slash command named after its
directory path.  Type `/` followed by the skill name in the input box:

```
/sven
/sven/plan
/git-workflow
/docker/compose
```

You can optionally follow the command with a task description on the same line:

```
/sven implement the rate-limiting feature described in the issue
/sven/plan analyse the authentication module
/git-workflow rebase my branch onto main
```

When you submit, sven receives the skill's full instruction set as your message,
with your task appended at the end.  The skill guides the agent's behaviour for
the rest of that turn.

---

### Hierarchical skills

Skills can be nested.  A top-level skill like `sven` describes a high-level
workflow and lists the sub-skills that handle each phase.  Each sub-skill is
also a fully independent slash command:

```
/sven               run the full three-phase workflow
/sven/plan          run only the planning phase
/sven/implement     run only the implementation phase
/sven/review        run only the review checklist
```

When the model loads a parent skill, it receives a compact list of the
available sub-skills.  It then calls `load_skill("sven/plan")` etc. exactly
when it enters each phase — not before.  This means sub-skill instructions are
loaded only when actually needed, keeping each turn's token usage minimal.

---

### Where to put skills

Sven looks for skills in the following locations, with later sources taking
precedence when the same command exists in multiple places:

| Location | When to use |
|----------|-------------|
| `~/.sven/skills/` | Your personal skills, available in every project |
| `~/.agents/skills/` | Cross-agent skills shared with other agents |
| `<project>/.sven/skills/` | Skills specific to this project |
| `<project>/.agents/skills/` | Project skills shared with other agents |

Project-level skills always win over global ones.

---

### Creating a skill

A skill is a directory containing a `SKILL.md` file:

```
.sven/skills/
└── deploy/
    ├── SKILL.md
    └── scripts/
        └── pre-flight.sh
```

`SKILL.md` starts with a YAML frontmatter block followed by the instruction
body:

```markdown
---
description: |
  Use this skill when the user asks to deploy, release, or ship the application.
  Trigger phrases: "deploy", "release", "ship to production".
name: Deploy             # optional — defaults to directory name
version: 1.0.0           # optional
---

# Deploy

Before deploying, run the pre-flight checklist in scripts/pre-flight.sh.

1. Confirm the target environment with the user.
2. Run `scripts/pre-flight.sh` and fix any failures.
3. Build the release artefact.
4. Push and tag.
```

The `description` field is the only required frontmatter key.  The model reads
it to decide whether to use the skill, so write it as a list of trigger phrases
and use-cases rather than a technical summary.

---

### Creating a hierarchical skill

Nest sub-skill directories inside the parent:

```
.sven/skills/
└── deploy/
    ├── SKILL.md              /deploy
    ├── pre-flight/
    │   └── SKILL.md          /deploy/pre-flight
    └── rollback/
        └── SKILL.md          /deploy/rollback
```

In the parent `SKILL.md`, tell the model to load sub-skills at the right time:

```markdown
---
description: |
  Full deployment workflow. Use when deploying to any environment.
---

# Deploy Workflow

Follow these phases in order:

1. Pre-flight checks — call `load_skill("deploy/pre-flight")` before touching
   any infrastructure.
2. Deploy the artefact.
3. If anything fails — call `load_skill("deploy/rollback")` immediately.
```

Sub-skills are automatically listed to the model when the parent is loaded, so
you do not need to declare them in the frontmatter.  Just create the directory.

---

### Frontmatter reference

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `description` | string | **yes** | Trigger phrases and use-cases. The model matches this against the user's request. |
| `name` | string | no | Human-readable label shown in the UI. Defaults to the directory name. |
| `version` | string | no | Semver version string for your own tracking. |
| `sven.always` | bool | no | Always include this skill's metadata in the system prompt, regardless of the token budget. Useful for a project-wide coding-style skill. Default: `false`. |
| `sven.requires_bins` | list | no | Skip this skill if any of the listed binaries are absent from `PATH` (e.g. `[docker, kubectl]`). |
| `sven.requires_env` | list | no | Skip this skill if any of the listed environment variables are unset (e.g. `[AWS_PROFILE]`). |
| `sven.user_invocable_only` | bool | no | Hide the skill from the model's automatic matching. It still appears as a `/command`. Use for skills you always want to invoke deliberately. Default: `false`. |

---

### Bundled files

Any file in a skill directory that is not a `SKILL.md` is a **bundled file** —
a script, reference document, template, or data file the skill's instructions
may use.  Subdirectories without their own `SKILL.md` are support directories,
not sub-skills.

When a skill is loaded via `load_skill`, the agent receives a listing of up to
20 bundled file paths relative to the skill directory.  The skill body can
reference them:

```markdown
Run the helper at `scripts/validate.py` before proceeding.
```

The agent resolves that path against the base directory shown in the tool
response and reads the file with `read_file`.

---

### Tips

**Write descriptions as trigger phrases.**  The model matches the description
against what the user asked.  `"Use when deploying to production"` is more
useful than `"Deployment skill"`.

**Keep parent bodies short.**  The parent skill should describe the workflow
and tell the model *when* to call each sub-skill.  Put the detailed instructions
in the sub-skills.

**Use `always: true` sparingly.**  Skills marked `always` are included in every
system prompt.  Reserve this for genuinely project-wide rules (e.g. a coding
style guide) rather than task-specific workflows.

**Use `user_invocable_only: true` for personal workflows.**  If a skill
contains steps you always want to review before running (e.g. a production
deployment), mark it `user_invocable_only` so the model never triggers it
automatically.

**Override globals with project skills.**  A project-level skill at
`.sven/skills/deploy/` silently replaces any global skill with the same command.
This lets you tailor shared skills for a specific repository.
