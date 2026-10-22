# Interactive use

The terminal interface, sessions, slash commands and the keys that drive them.

## The TUI in depth

### Layout

The TUI has a status bar, the chat pane, an input box, an agents panel that
appears under the chat while the session has subagents, and a queue panel that
appears above the input box while messages are waiting. The chat pane always spans
the full terminal width.

```
┌─────────────────────────────────────────────────────────────┐
│ ⬡ sven │ ⠿ gpt-4o │ agent │ ctx ██░░░ 18%  in: 32k out: 1k  │  status bar
├─────────────────────────────────────────────────────────────┤
│                                                              │
│   chat pane                                                  │
│   (scrollable conversation history)                          │
│                                                              │
├─────────────────────────────────────────────────────────────┤
│ > type here and press Enter                                  │  input box
└─────────────────────────────────────────────────────────────┘
```

**Status bar** - always visible at the top. From left to right:
- A spinner while the agent is working
- The model name
- The current agent mode (`research`, `plan`, `agent`, `chat` or `sdlc`)
- Context usage as a bar and a percentage
- Cumulative input and output token counts, the cache-hit rate and the cost in
  USD once the provider has reported them
- The name of any tool currently running
- On the right, a hint for the focused pane

**Chat pane** - the conversation history: your messages, the agent's answers
and the tools it ran. Tools the agent ran one after another, with no text of
its own in between, are folded into one row, `▸ Ran 4 tools · 1 failed ·
3.2s`; a single tool shows as its own row (icon, what it worked on, `✓` or
`✗`, and how long it took). Expand the run to see a row per tool, and expand
a tool to see what it was asked to do - the command, the path, an edit's
diff - and its output. Nothing expands to more than one chat pane's height:
a longer view ends in `… N more lines · o: open in viewer`, and `o` shows all
of it full screen. See [tools in the chat](#tools-in-the-chat). The pane
scrolls independently of the input box. The model's thinking is not drawn
unless `tui.show_thinking` is set; while it streams, the status bar says
`Thinking`.

**Input box** - a multi-line text field. `Enter` sends the message, `Alt+Enter`
inserts a newline (`Shift+Enter` and `Ctrl+J` also work). `Ctrl+Up` / `Ctrl+Down`
walk through the history of messages you sent.

**Agents panel** - one line summing up the session's subagents, `▸ Agents · 2
running · 1 done · 1m 14s · 12.3k tok`; open it for a row per agent. See [the
agents panel](#the-agents-panel).

**Queue panel** - a message sent while the agent is working is queued and sent
when the turn ends. See [the queue panel](#the-queue-panel).

Press `F1` for an overlay that lists the key bindings.

---

### Focus and pane switching

The chat pane, the agents and queue panels (while they are shown) and the input
box can each have focus; focus starts on the input box. Switch with the `Ctrl+W`
chord:

| Sequence | Effect |
|----------|--------|
| `Ctrl+W` then `K` or `↑` | Focus the pane above |
| `Ctrl+W` then `J` or `↓` | Focus the pane below |
| `Ctrl+W` then `A` | Focus the agents panel |
| `Ctrl+W` then `+` / `-` | Grow / shrink the input box |

When the input box has focus, printable characters go to the text field.

---

### Scrolling and navigation (chat pane)

First move focus to the chat pane with `Ctrl+W K` (once more for each panel
between the input box and the chat).

| Key | Action |
|-----|--------|
| `j` / `↓` | Move the highlight to the next entry |
| `k` / `↑` | Move the highlight to the previous entry |
| `Space` / `Enter` | Expand or collapse the highlighted entry (`Enter` on a message edits it) |
| `l` / `→` | Expand; on an expanded run, move into it |
| `h` / `←` | Collapse; on a collapsed tool, fold its run back up |
| `o` | Open the highlighted entry in full in the viewer |
| `J` / `K` | Scroll down / up one line |
| `Ctrl+D` / `Ctrl+U` | Scroll half a page down / up |
| `g` / `G` | Jump to the very top / bottom |
| `y` / `Y` | Copy the highlighted entry / the whole chat to the clipboard |
| `?` | Show the chat-pane shortcuts |

`PgUp` and `PgDn` scroll the chat pane a full page whichever pane has focus.
While you are scrolled up, the view stays on what you are reading: output that
arrives, and tools above you that finish or are expanded, do not move it. The
bottom row says how many lines are below (`↓ 23 lines below · G to follow`);
`G`, or scrolling to the end, follows the output again.
Dragging with the mouse selects text, which is copied when you release.

---

### Tools in the chat

The highlight visits what is drawn: messages, the header of a run of tools,
and - once the run is expanded - each tool in it. Clicking an entry
highlights it and expands or collapses it, the same as `Space`.

```
▸ Ran 3 tools · 0.4s                  collapsed run

▾ Ran 3 tools · 0.4s                  expanded run
  ▾ ▶ read_file  src/auth.rs  ✓  0.1s   expanded tool: its arguments,
      src/auth.rs                       then its output
      use crate::tokens;
      … 2413 more lines · o: open in viewer
  ▸ $ shell  cargo test  ✗  0.3s      collapsed tool
```

While a tool runs, its row shows a moving marker and the time so far, the
run's header names the tool that is running, and an expanded tool shows the
newest output it has streamed. Every expanded entry is at most one chat pane
tall, so expanding a large file or log never pushes the conversation out of
reach; `o` opens the whole of it in the full-screen viewer, and closing the
viewer returns to the chat exactly where it was.

---

### Search

Press `/` in the chat pane to open the search bar at the bottom of the screen.
Type a query; the view jumps to the match.

| Key | Action |
|-----|--------|
| `/` | Open search |
| `n` | Jump to next match |
| `N` | Jump to previous match |
| `Esc` or `Enter` | Close search and stay on the current match |

---

### Editing, removing and rerunning messages

With an entry highlighted in the chat pane:

| Key | Action |
|-----|--------|
| `e`, `Enter` or `F2` | Edit it: its text moves to the input box. `Enter` confirms, `Esc` cancels and restores the original. (`Enter` edits only messages; on anything else it expands or collapses.) |
| `d` or `F8` | Truncate the chat from this entry onward |
| `x` | Remove only this entry (and its paired tool call or result) |
| `r` | Truncate to just before this entry and send the conversation to the agent again |

Confirming an edit of one of your messages replaces it, drops everything after it,
and runs the agent from the edited message.

---

### The queue panel

A message you send while the agent is working appears in a panel above the input
box and is sent when the turn ends. Press `q` in the chat pane to focus the panel.

| Key | Action |
|-----|--------|
| `↑` / `↓` or `k` / `j` | Select a queued message |
| `e` | Edit the selected message |
| `Enter` or `f` | Send it now, aborting the running turn |
| `s` | Send it when the agent is idle |
| `d` or `Delete` | Delete it |
| `Esc` or `q` | Return to the input box |

---

### The agents panel

While the session has subagents (started by the `task` tool), a line under the
chat sums them up: how many are running, done and failed, how long they have
taken from the first start to the last end, and the tokens they used. Press
`Ctrl+W A` to focus it.

| Key | Action |
|-----|--------|
| `Enter` or `Space` | Open the panel: a row per agent |
| `↑` / `↓` or `k` / `j` | Select an agent |
| `Enter` | Show the selected agent's session - its transcript, live |
| `Esc` or `q` | Fold the panel and return to the input box |

A row shows the agent's task, the tool it is running (or `done`, or why it
failed), how many tools it ran, how long it has been running and its tokens.
Tokens add up from each request's reported usage while the agent runs and are
replaced by its own totals when it finishes. The open panel takes at most two
fifths of the screen; with more agents than rows it scrolls, with a scrollbar on
the right. Clicking the summary line opens or folds the panel, and clicking an
agent's row shows its session. The panel lists the subagents of the session
being viewed, or - while one of them is viewed - of its parent, so you can move
between siblings. Subagents restored from a saved session are reachable through
`/resume`, not the panel.

---

### Full-screen pager

Press `Ctrl+T` to open the chat history full screen, which is useful for reading
long responses or code blocks. It scrolls like `less`: `j` / `k`, `Ctrl+D` /
`Ctrl+U`, `Space`, `PgUp` / `PgDn`, `gg` / `G`, and `/` to search. `Esc` or `q`
closes it. The inspector commands (`/skills`, `/tools` and the others below) show
their output in the same pager.

---

### Neovim integration

By default the chat pane is plain ratatui. Pass `--nvim` to embed a Neovim
instance as the chat buffer instead:

```sh
sven --nvim
```

This needs `nvim` on your `PATH`. The chat pane then has full Neovim editing
and motions, `:q` or `:qa` quits sven, and `Ctrl+Enter` in the chat pane submits
the buffer content as a message.

---

### Teams

When a team is active, `Ctrl+A` opens the team picker (also `/agents`),
`Shift+Down` / `Shift+Up` cycle between teammate views, and `Alt+T` shows the task
list (also `/tasks`). See [Teams](teams.md).

---

## Modes and approval

The status bar shows the current mode. `F4` cycles
`research → plan → agent → chat → sdlc`; `/mode <name>` sets one directly. The
change applies from the next message. Start with a mode using `--mode`.

By default every call the mode allows runs without a prompt. Start the TUI with
`--approval manual` and each call that is not read-only opens a `yes` / `no` modal
first. The full rules for both are in [Agents and modes](../concepts/agents.md).

---

## Sessions

Each conversation is a **session**: its own history, agent task, model and mode.
`/new` starts an empty session using the model from your configuration and the
`agent` mode. The previous session is saved and keeps running in the background if
a turn was in progress. `/resume` opens a full-screen list of saved sessions
(newest first); `↑` / `↓` or `j` / `k` select, `Enter` loads one, `Esc` or `q`
closes the list.

Changing the model or mode with `/model` or `/mode` affects only the current
session, and takes effect on the next message you send.

Sessions are saved automatically after each turn as ATIF trajectory files
(`.json`) in the sven data directory, `~/.local/share/sven/sessions/` on Linux.
`sven chats` prints the exact directory.

### Resuming from the command line

List saved sessions:

```sh
sven chats            # the 20 most recent; -n N for a different count
```

```
ID (use with --resume)                  UPDATED           STATUS     TITLE
-----------------------------------------------------------------------------------------------
3f4a...                                 2026-01-15 10:42  Completed  Codebase analysis
a1b2...                                 2026-01-14 09:11  Active     Rate limiter design
```

Pass an ID, a unique prefix of one, or a path to `--resume`:

```sh
sven --resume 3f4a
```

`--resume` with no ID opens the TUI with the session list showing. Headless runs
need an explicit ID, and a headless resume appends to the same session.

### Conversation files

A conversation file is a plain-text record of a session that you can edit. See
[Quick Start](../getting-started/quickstart.md) for an introduction and
[CI and Pipelines](headless.md) for the full file format.

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
| `--mode <machine>` | Which machine to run (default `agent`; `chat`, `sdlc` and the other registered machines are accepted). Only used when starting fresh; a resumed agent keeps its own. |
| `--role <text>` | A stable system prompt for the agent. |

A step runs unattended, like every non-interactive run: every tool call the
agent makes runs without a prompt, and a question the agent asks is answered
at once ("No user is available to answer this question. Proceed on your best
judgement and state the assumption you made."). `--mode` here names a machine;
the read-only `research` and `plan` modes of the main command are not available.

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

sven tracks context usage and shows it in the status bar. Before each request,
when usage reaches `agent.compaction_threshold` of the usable input budget (0.85
by default) less the part of the window kept free for the response
(`agent.compaction_output_reserve`, 0.25), sven asks the model to write notes on
the conversation and replaces the history with them; the task and the todo list
are kept verbatim. This happens transparently, and a `context budget:` line at
the start of the session says at what size.

Compaction changes how much raw history the model sees at once; the checkpoint
carries the tasks, decisions, files and constraints forward. The full conversation
stays in the saved session.

---

## Interrupting the agent

To stop a turn in progress, press `Ctrl+C` or `Esc` in the input box, or run
`/abort`. The current turn is cancelled, partial output is kept, queued messages
stay queued, and sven returns to idle. `Ctrl+C` does not quit sven; use `/quit`.

---

## Slash commands

Type `/` in the input box to see all available commands. Tab completes command
names and arguments, and `Shift+Tab` cycles backwards.

### Built-in commands

| Command | Description |
|---------|-------------|
| `/new` | Start a new session. |
| `/resume` | Browse saved sessions and load one. |
| `/clear` | Clear the current session's chat history. The session stays open. |
| `/model <provider/name>` | Switch the model (e.g. `/model anthropic/claude-opus-4-6`). Tab-completes over the configured providers and the built-in model catalog. Takes effect on the next message. |
| `/provider <name>` | Switch provider, keeping the current model name. Takes effect on the next message. |
| `/mode <research\|plan\|agent\|chat\|sdlc>` | Switch the agent mode for this session. Takes effect on the next message. |
| `/abort` | Abort the current agent turn. Queued messages stay queued; partial output is kept. |
| `/think-limit [<max_tokens> [<timeout_secs>]] \| off` | Show or set the watchdog that stops a model stuck in a thinking loop. With no argument, shows the current limit. |
| `/refresh` | Re-apply the MCP server configuration and refresh skills. |
| `/skills` | Open the skills inspector, a browsable tree of loaded skills. |
| `/subagents` | Show configured subagents with their descriptions, models and paths. |
| `/peers` | Show configured subagents and active subprocess buffers. |
| `/context` | Show the agent context: project root, skill and agent counts, output buffer handles. |
| `/tools` | Show all available tools with descriptions and parameter counts. |
| `/mcp [list\|enable\|disable\|auth] [name]` | Show and manage MCP servers; `auth` starts OAuth sign-in for a server. |
| `/architect [request]` | Send a message that asks the agent to plan and specify changes and delegate the implementation to an editor agent. |
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

Each subagent definition is also a command named after the agent (lowercased).
It sends the agent's instructions with your text appended as `Task: <text>`, and
switches to the agent's model when it names one. Prompts exposed by connected MCP
servers appear as commands too.

Skills are not slash commands - see the [Skills](skills.md) section.
