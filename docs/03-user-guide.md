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

If your terminal does not support the Neovim embed, or if you simply prefer the
plain ratatui view, pass `--no-nvim`:

```sh
sven --no-nvim
```

In `--no-nvim` mode, tool calls and thinking blocks in the history are collapsed
by default to keep the view compact.

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

sven includes integrated GDB debugging support aimed at embedded development
workflows. The five GDB tools form a lifecycle:

```
gdb_start_server → gdb_connect → gdb_command / gdb_interrupt → gdb_stop
```

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
