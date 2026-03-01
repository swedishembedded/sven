# Quick Start

This guide gets you running in about five minutes. It assumes you have sven
installed and an API key in your environment. If not, see
[Installation](01-installation.md) first.

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
│ gpt-4o  agent  ctx:12%                          │  ← status bar
├─────────────────────────────────────────────────┤
│                                                  │
│  (conversation and tool output appears here)     │  ← chat pane
│                                                  │
├─────────────────────────────────────────────────┤
│ > _                                              │  ← input box
└─────────────────────────────────────────────────┘
```

- **Status bar** — shows the active model, current mode, and how much of the
  context window is used.
- **Chat pane** — the conversation history and streaming agent output.
- **Input box** — where you type your messages.

Focus switches between the chat pane and the input box. When the input box has
focus (the default), your keystrokes go to the text field. When the chat pane
has focus, vim-style navigation keys let you scroll through the history.

---

## Essential keyboard shortcuts

### Input box (focused by default)

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Shift+Enter` | Insert a newline (multi-line message) |
| `Ctrl+C` | Interrupt a running agent turn |
| `Ctrl+U` | Delete from cursor to start of line |
| `Ctrl+K` | Delete from cursor to end of line |
| `Ctrl+←` / `Ctrl+→` | Jump word left / right |

### Chat pane

Switch focus from the input box to the chat pane with `Ctrl+W` then `K`.
Switch back with `Ctrl+W` then `J` (or just click the input box).

| Key | Action |
|-----|--------|
| `j` / `↓` | Scroll down one line |
| `k` / `↑` | Scroll up one line |
| `Ctrl+D` | Scroll down half a page |
| `Ctrl+U` | Scroll up half a page |
| `g` | Jump to top |
| `G` | Jump to bottom |
| `/` | Open search |
| `n` / `N` | Next / previous search match |
| `e` | Edit the message under the cursor |

### Global shortcuts

| Key | Action |
|-----|--------|
| `F1` | Toggle help overlay |
| `F4` | Cycle through agent modes (research → plan → agent) |
| `Ctrl+T` | Open the full-screen pager (review chat history) |

To quit, type `/quit` in the input box, or use `:q` in the Neovim buffer.

---

## Starting with a prompt

Pass a prompt directly on the command line to skip the empty input box:

```sh
sven "List the ten largest files in this repository."
```

sven opens the TUI, pre-fills the prompt, and submits it immediately.

---

## Headless mode (no TUI)

Sven runs headlessly — without a TUI — in two situations:

- **Piped stdin**: when stdin is not a terminal, sven auto-detects this and
  skips the TUI automatically.
- **`--headless` flag**: required when invoking sven directly from a terminal
  prompt without piping, so that sven writes its response to stdout instead of
  opening the interactive TUI.

```sh
# Piped — headless is automatic
echo "Summarise the README in three bullet points." | sven

# Terminal prompt — --headless is mandatory to avoid the TUI
sven --headless "Summarise the README in three bullet points."
```

The output is plain text — easy to pipe into other tools or save to a file:

```sh
echo "Summarise the README." | sven > summary.txt
sven --headless "Summarise the README." > summary.txt
```

### Suppressing formatting with `--output-format compact`

By default headless output is conversation-format markdown (with `## User` /
`## Sven` headings).  Pass `--output-format compact` to get only the agent's
raw response text with no surrounding markup — useful when you want to capture
or execute the result directly.

Combine `--headless`, `--output-format compact`, and `2>/dev/null` to get
output that can be used directly in a shell pipeline:

```sh
# Ask sven for a shell command and execute it immediately
CMD=$(sven --headless --output-format compact \
      "Generate a one-liner for CPU usage. Reply with the shell command only." \
      2>/dev/null)
eval "$CMD"

# Or pipe the command text to sh
sven --headless --output-format compact \
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
sven --file work.md --conversation
```

sven reads the `## User` section, executes it, then appends the response as a
new `## Sven` section. The file now contains both your message and the answer.
Open it in any text editor to read, edit, and continue:

```sh
# Append a follow-up question
printf '\n## User\nWhich files have the most technical debt?\n' >> work.md

# Run again — sven loads the history and answers only the new question
sven --file work.md --conversation
```

---

## Choosing a mode

Add `--mode` to limit what sven is allowed to do:

```sh
# Research only — no changes to your files
sven --mode research "What does the auth module do?"

# Plan — produces a written plan but makes no changes
sven --mode plan "Design a caching layer for the database module."

# Agent (default) — full read/write access
sven "Implement the caching layer we just designed."
```

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
sven --file review.md
```

Output is valid conversation markdown that can be piped or loaded later:

```sh
# Pipe into another instance for follow-up analysis
sven --file review.md | sven "Summarise the main findings in one sentence."
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

- [User Guide](03-user-guide.md) — full details on the TUI, modes, tools, and
  conversation management
- [CI and Pipelines](04-ci-pipeline.md) — run sven in scripts and automated
  workflows, with workflow file format reference, output formats, timeouts,
  artifacts, and CI integration guides
- [Configuration](05-configuration.md) — change the model, set defaults, and
  tune behaviour
