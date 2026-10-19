# Interactive use

The terminal interface, conversations, slash commands and the keys that drive them.

## The TUI in depth

### Layout

The TUI has four visual regions. The chat list sidebar on the right is optional
and can be toggled with `Ctrl+B`:

```
┌─────────────────────────────────────────┬───────────────────┐
│ gpt-4o  agent  ctx:18%  ⠿ run_terminal │ ● New chat        │  status bar
├─────────────────────────────────────────┤ ○ Rate limiter    │
│                                         │ ○ Codebase review │
│   chat pane                             │ ✓ Auth redesign   │
│   (scrollable conversation history)     │                   │
│                                         │ n:new d:del a:arch│
├─────────────────────────────────────────┤                   │
│ > type here and press Enter             │                   │
└─────────────────────────────────────────┴───────────────────┘
```

**Status bar** - always visible at the top. Shows:
- Model name (e.g. `gpt-4o`)
- Current agent mode (`research`, `plan`, or `agent`)
- Context usage as a percentage (`ctx:18%`)
- A spinner and the name of any tool currently running

**Chat pane** - the conversation history. User messages, agent responses, and
collapsed tool calls are all shown here. Scrolls independently of the input box.

**Input box** - a multi-line text field. Press `Enter` to send, `Shift+Enter`
to insert a newline.

**Chat list sidebar** - shows all open sessions. The active session is
highlighted. Sessions running a background agent task show a spinner. Toggle
with `Ctrl+B`.

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

### Chat list sidebar

The chat list sidebar lets you manage multiple concurrent conversations without
leaving the TUI. Each session runs its own independent agent task - you can
have one session researching a problem while another is implementing a fix.

**Opening and navigating the sidebar**

Press `Ctrl+B` to show the sidebar and give it keyboard focus. Press `Ctrl+B`
again (or `Esc`, `q`, `h`, or `←`) to return focus to the input box.

| Key | Action |
|-----|--------|
| `Ctrl+B` | Toggle sidebar visibility / focus |
| `j` / `↓` | Move selection down |
| `k` / `↑` | Move selection up |
| `Enter` / `l` / `→` | Switch to the selected session |
| `n` | Create a new empty session |
| `d` / `Delete` | Delete the selected session (cannot delete the active one) |
| `a` | Archive the selected session |
| `+` / `=` | Widen the sidebar |
| `-` | Narrow the sidebar |

**Session status icons**

| Icon | Meaning |
|------|---------|
| `⣾` (spinner) | Agent is currently running in this session |
| `●` | Active session, idle |
| `○` | Inactive session, idle |
| `✓` | Session completed |

**Per-session model and mode**

Each session keeps its own model and mode. Running `/model` or `/mode` in one
session does not affect any other session. New sessions always start with the
default model and mode from your configuration.

**Background sessions**

When you switch away from a session that has an agent running, it keeps running
in the background. The spinner in the sidebar shows which sessions are still
active. You can switch back at any time to see the results.

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

## Conversation management

### Multiple sessions inside the TUI

You can run several conversations simultaneously without restarting sven. Open
the chat list sidebar with `Ctrl+B` and press `n` to create a new session, or
use the `/new` command from the input box. Switch between sessions by selecting
one and pressing `Enter`.

Each session has its own:
- Conversation history
- Agent task (background sessions keep running while you work elsewhere)
- Model and mode settings (changed with `/model` and `/mode`)

All sessions are automatically saved to `~/.config/sven/history/` as YAML files
and restored the next time you launch sven.

### Starting and continuing conversations from the CLI

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
you can edit directly. See [Quick Start](../getting-started/quickstart.md) for an introduction,
and [CI and Pipelines](headless.md) for the full file format.

---

## Scripting an agent a step at a time

`sven agent step` runs an agent for exactly one turn and exits, keeping
everything durable in a state file:

```sh
sven agent step --state ./review.json "read src/lib.rs and summarise it"
sven agent step --state ./review.json "now list its public types"
```

Each command is a separate process. The agent is loaded from the file,
advanced by one step, written back, and dropped - so a shell script, a cron
job or a web handler can drive a long conversation without holding anything
open between turns.

| Flag | Meaning |
|------|---------|
| `--state <file>` | Load from and write back to this file. Omit it for a one-off step that keeps nothing. |
| `--mode <mode>` | Which machine to run. Only used when starting fresh; a resumed agent keeps its own. |
| `--role <text>` | A stable system prompt for the agent. |

A step runs unattended, like every non-interactive run: every tool call its
mode allows runs without a prompt, and a question the agent asks is answered
at once ("No user is available to answer this question. Proceed on your best
judgement and state the assumption you made."). Pick a read-only `--mode`
where the agent must not change anything.

A state file that exists but cannot be read is an error rather than a fresh
start: silently discarding a conversation would only show up later, as an agent
that had inexplicably forgotten everything.

To build this into your own program rather than script it, see
[the SDK](../reference/sdk.md).

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

## Slash commands

Type `/` in the input box to see all available commands. Tab-completion works
for both command names and their arguments.

### Built-in commands

| Command | Description |
|---------|-------------|
| `/new` | Start a new chat session. A fresh tab appears in the sidebar with its own isolated agent, model, and mode. |
| `/clear` | Clear the current session's message history. The session itself stays open; only the visible conversation is erased. |
| `/model <provider/name>` | Switch the model for this session (e.g. `/model anthropic/claude-opus-4-6`). Tab-completes over your configured models. The switch takes effect on the next message you send. |
| `/mode <research\|plan\|agent>` | Switch the agent mode for this session. Tab-completes all three modes. |
| `/provider <name>` | Switch provider while keeping the current model name. |
| `/abort` | Abort the current agent turn. Queued messages stay queued; partial output is preserved. |
| `/refresh` | Re-scan skill directories and register any newly added skills as commands. |
| `/skills` | Open the skills inspector - a browsable tree of all loaded skills. |
| `/subagents` | Show all configured subagents with their descriptions, models, and paths. |
| `/peers` | Show active subagent subprocess buffers and configured team agents. |
| `/context` | Show the current agent context: project root, skill and agent counts, output buffer handles. |
| `/tools` | Show all available tools with descriptions and parameter counts. |
| `/approve [task_id]` | Approve a teammate's pending plan (team mode). |
| `/reject [task_id] [reason]` | Reject a plan with feedback (team mode). |
| `/agents` | Show the team members overlay (also `Ctrl+A`). |
| `/tasks` | Show the current team task list (also `Alt+T`). |
| `/quit` | Exit sven. In the Neovim buffer, use `:q` or `:qa`. |

### Custom commands

Every markdown file in a `commands/` directory (`.sven/commands/`,
`.cursor/commands/`, `.claude/commands/`, `.codex/commands/` or
`.agents/commands/`, in your project or a parent directory) is a slash command
named after its path without the `.md` extension - for example
`.sven/commands/review-code.md` → `/review-code` and
`.sven/commands/sven/plan.md` → `/sven/plan`. Submitting the command sends the
file's content as your message; any text after the command is appended as
`Task: <text>`:

```
/sven/plan analyse the authentication module
```

Skills are not slash commands - see the [Skills](skills.md) section.
