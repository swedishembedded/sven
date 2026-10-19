# Quick Start

This guide gets you running in about five minutes. It assumes you have sven
installed and an API key in your environment. If not, see
[Installation](installation.md) first.

---

## Check that sven can reach a model

Before the first session, ask sven whether the model it would use is reachable. It
builds the provider the way a session does and reports each step, with what to change
when one fails:

```sh
sven doctor provider
```

```
ok   provider: brain/Qwen/Qwen3-0.6B
ok   endpoint: http://127.0.0.1:8788/v1 serves 23 model(s)
ok   model: Qwen/Qwen3-0.6B is served
ok   tools: the server advertises tool calling
ready: sven can use this model
```

With a hosted provider it reports only that the provider could be built and which key it
found missing, and sends nothing. [Troubleshooting](../guides/troubleshooting.md) explains each
check.

---

## Your first interactive session

Open a terminal in any project directory and run:

```sh
sven
```

The TUI starts with the input box focused at the bottom of the screen. Type
your task and press `Enter` to send it.

```
Explain what this project does and list its main entry points.
```

sven will start working immediately: you will see it read files, run commands,
and stream its response into the chat pane above. When it finishes, type
another message to continue the conversation, or type `/quit` to exit.

---

## The TUI layout

The screen is divided into three areas:

```
┌─────────────────────────────────────────────────┐
│ ⬡ sven │ gpt-4o │ agent │ ctx ██░░░ 12%         │  ← status bar
├─────────────────────────────────────────────────┤
│                                                  │
│  (conversation and tool output appears here)     │  ← chat pane
│                                                  │
├─────────────────────────────────────────────────┤
│ > _                                              │  ← input box
└─────────────────────────────────────────────────┘
```

- **Status bar** - shows the active model, the current mode, how much of the
  context window is used, token counts and cost once a turn has run, and the tool
  that is currently running.
- **Chat pane** - the conversation history and streaming agent output. Tool calls
  and thinking blocks are collapsed to a summary.
- **Input box** - where you type your messages. Messages you send while the agent
  is working wait in a queue panel shown above the input box.

Focus switches between the chat pane and the input box. When the input box has
focus (the default), your keystrokes go to the text field. When the chat pane
has focus, vim-style navigation keys let you scroll through the history.

---

## Essential keyboard shortcuts

### Input box (focused by default)

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Alt+Enter` | Insert a newline (multi-line message); `Shift+Enter` and `Ctrl+J` also work |
| `Ctrl+C` / `Esc` | Interrupt a running agent turn (`Esc` on an idle box clears it) |
| `Ctrl+U` | Delete from cursor to start of line |
| `Ctrl+K` | Delete from cursor to end of line |
| `Ctrl+←` / `Ctrl+→` | Jump word left / right |

### Chat pane

Switch focus from the input box to the chat pane with `Ctrl+W` then `K`.
Switch back with `Ctrl+W` then `J`.

| Key | Action |
|-----|--------|
| `j` / `↓` | Move the highlight to the next entry |
| `k` / `↑` | Move the highlight to the previous entry |
| `Ctrl+D` / `Ctrl+U` | Scroll half a page down / up |
| `g` / `G` | Jump to top / bottom |
| `/` | Open search |
| `n` / `N` | Next / previous search match |
| `e` | Edit the message under the highlight |

`PgUp` and `PgDn` scroll the chat pane a full page from either pane.

### Global shortcuts

| Key | Action |
|-----|--------|
| `F1` | Toggle help overlay |
| `F4` | Cycle the agent mode: research → plan → agent → chat → sdlc |
| `Ctrl+T` | Open the full-screen pager (review chat history) |

To quit, type `/quit` in the input box, or use `:q` in the Neovim buffer
(started with `--nvim`). The full key and command lists are in
[Interactive use](../guides/interactive.md).

---

## Starting with a prompt

A prompt on the command line runs one task without the TUI and prints the result:

```sh
sven "List the ten largest files in this repository."
```

To work interactively, start `sven` with no prompt and type into the input box.

---

## Headless mode (no TUI)

Sven runs without the TUI whenever any of these holds:

- **A prompt is given** on the command line.
- **stdin is not a terminal** (a pipe or redirect). Without a positional prompt,
  stdin is the task. With one, stdin is read only when you pass `--stdin` (e.g.
  `cmd | sven --stdin "fix these errors"`): it is then appended to the prompt
  with a blank line and sent as one user message.  Without `--stdin`, a run
  that has a prompt never reads or waits on stdin.
- **stdout is not a terminal** (`sven ... > out.txt`, or the left side of a pipe).
- **`--headless`** is passed. It is only needed when none of the above applies,
  for example `sven --headless --file review.md` from a terminal.

```sh
# Prompt - headless is automatic
sven "Summarise the README in three bullet points."

# Piped - headless is automatic
echo "Summarise the README in three bullet points." | sven
```

The output is plain text - easy to pipe into other tools or save to a file:

```sh
echo "Summarise the README." | sven > summary.txt
sven "Summarise the README." > summary.txt
```

Headless runs are unattended: every call the mode permits runs without a prompt
and a question the agent asks is answered with "no user is available".
`--approval manual` is refused in a headless run. See
[Agents and modes](../concepts/agents.md#approval).

### Suppressing formatting with `--output-format compact`

By default headless output is conversation-format markdown (with `## User` /
`## Sven` headings).  Pass `--output-format compact` to get only the agent's
raw response text with no surrounding markup - useful when you want to capture
or execute the result directly.

Combine `--output-format compact` and `2>/dev/null` to get output that can be
used directly in a shell pipeline:

```sh
# Ask sven for a shell command and execute it immediately
CMD=$(sven --output-format compact \
      "Generate a one-liner for CPU usage. Reply with the shell command only." \
      2>/dev/null)
eval "$CMD"

# Or pipe the command text to sh
sven --output-format compact \
     "Generate a one-liner for CPU usage. Reply with the shell command only." \
     2>/dev/null | sh
```

---

## Your first conversation file

A conversation file is a plain markdown file that sven reads, executes, and
writes back to. It is a great way to iterate on a task across multiple sessions
without losing context.

Create a file called `work.md`:

```markdown
# My Project Analysis

## User
Describe the overall structure of this project.
```

Run sven on it:

```sh
sven --headless --file work.md
```

Without `--headless`, `sven --file work.md` from a terminal opens the TUI with the
file's steps queued for you to review and send. With `--headless`, sven reads the
`## User` section, executes it, and writes the exchange - your message and the
answer as a new `## Sven` section - to stdout. The file itself is not changed.
Redirect the output to keep the answer, add a follow-up, and
run it again:

```sh
sven --file work.md > work.answered.md   # stdout is redirected, so this is headless

# Append a follow-up question
printf '\n## User\nWhich files have the most technical debt?\n' >> work.answered.md

# Run again - sven loads the history and answers only the new question
sven --file work.answered.md
```

A file with `## User` / `## Sven` sections is recognised as a conversation; no
flag is needed. To keep one session across runs without juggling files, use
`--trace session.json`.

---

## Choosing a mode

Add `--mode` to choose what sven is allowed to do:

```sh
# Research - read-only tools, no shell, no file writes
sven --mode research "What does the auth module do?"

# Plan - produces a plan; the file-write tools are refused
sven --mode plan "Design a caching layer for the database module."

# Agent (default) - full read/write access
sven "Implement the caching layer we just designed."

# SDLC - phased Intake to Delivery workflow
sven --mode sdlc "Add a caching layer to the database module."
```

By default every call a mode permits runs without asking. Start the TUI with
`sven --approval manual` to approve each call that is not read-only. See
[Agents and modes](../concepts/agents.md).

---

## Workflow files

For multi-step tasks, write a markdown workflow file.  Each `##` heading is a
step (user message).  The first `#` H1 heading is the conversation title, and
any text before the first `##` is appended to the agent system prompt:

```markdown
# Code Review

Automated review workflow. Be concise and cite file/line numbers.

## Understand the change
<!-- sven: mode=research -->
Read the most recently modified files and summarise what changed.

## Review for issues
Identify bugs, security issues, and style violations.

## Write review
Produce a constructive code review.
```

Run it:

```sh
sven --headless --file review.md
```

Output is valid conversation markdown that can be piped or loaded later:

```sh
# Pipe into another instance for follow-up analysis
sven --file review.md | sven --stdin "Summarise the main findings in one sentence."
```

---

## Configuring sven

Create `~/.config/sven/config.yaml` to set your default model and other options:

```yaml
model:
  provider: openai
  name: gpt-4o
  api_key_env: OPENAI_API_KEY
```

To use Anthropic Claude instead:

```yaml
model:
  provider: anthropic
  name: claude-opus-4-5
  api_key_env: ANTHROPIC_API_KEY
```

See all available models:

```sh
sven list-models
```

---

## What next?

- [User Guide](../guides/interactive.md) - full details on the TUI, modes, tools, and
  conversation management
- [CI and Pipelines](../guides/headless.md) - run sven in scripts and automated
  workflows, with workflow file format reference, output formats, timeouts,
  artifacts, and CI integration guides
- [Configuration](../reference/configuration.md) - change the model, set defaults, and
  tune behaviour
