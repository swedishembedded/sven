<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Tools and approvals

Tools are what an agent calls to act: read and change files, run commands, search,
delegate, ask you a question. This page lists every tool sven can offer, what
each one needs, and exactly when a call runs without asking you. Agent modes
and approval are explained in [Agents and modes](../concepts/agents.md); the
configuration keys are in [Configuration](configuration.md).

## Which tools a session has

A session gets one of four built-in tool sets, chosen when it starts and fixed
for its lifetime:

| Tool set | When | Contents |
|----------|------|----------|
| coding | the default (every mode except `research`, when the project has no GDB configuration) | the file, search, shell, web, memory, skill, system, todo, `task` and `ask_question` tools |
| full | the project root holds GDB configuration (see [Debugging](../guides/debugging.md#when-the-gdb-tool-is-offered)) | coding, plus `context` and `gdb` |
| research | `--mode research` | read-only subset: no `write_file`, `edit_file`, `shell` or `attach_file`; `system` cannot manage MCP servers; `task` delegates only to read-only children |
| sub-agent | a `task` child process | coding without `task` and `ask_question`; no `context` or `gdb`; `system` cannot manage MCP servers |

Tools are added to or removed from the set by:

- **MCP servers** (`mcp_servers` in the configuration): each server's tools are
  added as `<server>-<tool>`. They are gated under network access, are never
  core tools, and can be hot-swapped while the session runs.
- **`tools.disabled`**: names listed there are removed last, so a disabled tool
  is neither offered nor run, whoever registered it.
- **`--no-tools`**: sends no tool definitions and refuses any call.
- **Cargo features** at build time: `media` (`attach_file`, images in
  `read_file`), `asr` (audio transcription), `gdb` (Unix only), `memory`
  (`semantic_memory`), `android` (the `android` tool and the `ui-test` mode).
  The default build has all of them.

## Tool reference

"Mode" is the `Tool::modes` list: the agent modes whose tool listing contains
the tool. The reactive machines behind `agent`, `plan`, `research` and `chat`
all request the `agent` listing; plan and research are kept read-only by the
kernel permission policy (and, for research, by the smaller tool set), not by
hiding definitions. SDLC phases name their tools explicitly (see
[Per-phase tools in `sdlc`](#per-phase-tools-in-sdlc)). "Disclosure" is `core`
for the tools always sent in full under deferred disclosure and `deferred` for
the rest ([Core and deferred disclosure](#core-and-deferred-disclosure)).

| Tool | Gated as | Mode | Disclosure |
|------|----------|------|------------|
| [`read_file`](#read_file) | read | all | core |
| [`find_file`](#find_file) | read | all | core |
| [`grep`](#grep) | read | all | core |
| [`write_file`](#write_file) | write | `agent` | core |
| [`edit_file`](#edit_file) | write | `agent` | core |
| [`shell`](#shell) | shell | all | core |
| [`todo`](#todo) | read | all | core |
| [`tools`](#tools) | read | all | core |
| [`attach_file`](#attach_file) | read | all | deferred |
| [`web_fetch`](#web_fetch) | network | all | deferred |
| [`web_search`](#web_search) | network | all | deferred |
| [`task`](#task) | read | `agent` | deferred |
| [`memory`](#memory) | read or write, per action | all | deferred |
| [`semantic_memory`](#semantic_memory) | read or write, per action | all | deferred |
| [`skill`](#skill) | read | all | deferred |
| [`system`](#system) | per action | all | deferred |
| [`ask_question`](#ask_question) | read | all | deferred |
| [`context`](#context) | read or network, per action | `agent`; all three with expansion | deferred |
| [`gdb`](#gdb) | shell | `agent` | deferred |
| [`android`](#android) | device | all | deferred |

"Gated as" is the kernel capability a call is checked under (see
[Approval](#approval)); "per action" means the `action` argument decides it, and
an unknown or missing action is held to the tool's widest capability. Every
`path` argument of the file tools resolves through the session's path scope: a
confined embedding refuses anything outside its root.

### `read_file`

Read a file or a line range of it. Images (png, jpg, gif, webp, bmp, tiff) come
back as an image in builds with the `media` feature.

| Parameter | Description |
|-----------|-------------|
| `path` (required) | File path |
| `offset` | First line, 1-indexed (default 1) |
| `limit` | Maximum lines (default 200) |

### `find_file`

Find files by name glob under a directory, recursively; names only. A pattern
containing `/` matches the path relative to `root`; one without it matches the
file name anywhere below. Skips `.git`, `target`, `node_modules` and `.cargo`.

| Parameter | Description |
|-----------|-------------|
| `pattern` | Glob such as `*.rs`; omit to list everything |
| `root` | Directory to search (default: working directory) |
| `case_insensitive` | Default `false` |
| `max_results` | Default 40; the reply says when more matched |
| `timeout_secs` | Seconds before the search stops (default 10) |

### `grep`

Search file contents by regular expression (ripgrep).

| Parameter | Description |
|-----------|-------------|
| `pattern` (required) | Regular expression |
| `path` | File or directory (default: working directory) |
| `include` | File-name glob filter |
| `whole_project` | Skip `.git`, `target`, `node_modules`, `dist`, `__pycache__` and `*.lock` (default `false`) |
| `case_sensitive` | Default `true` |
| `limit` | Maximum matches (default 100) |
| `output_mode` | `content` (default, `file:line:col:text`), `files_with_matches` or `count` |
| `context_lines` | Lines around each match in `content` mode (default 0) |

### `write_file`

Write a whole file, creating missing parent directories. Overwrites unless
`append` is true.

| Parameter | Description |
|-----------|-------------|
| `path` (required) | File path |
| `text` (required) | Content to write |
| `append` | Append instead of overwriting (default `false`) |

### `edit_file`

Edit an existing file by applying unified diff hunks. Every hunk starts with an
`@@` line; context lines must match the file. A hunk is located by an exact
match, then by a match that differs only in one common indentation offset, then
by a fuzzy match in which every line must resemble the file line it lands on
(a typo passes; a missing context line does not, because the hunk would line up
a line late). The offset found is applied to the added lines; an offset of one
column is taken for context lines that lack their leading space and is not
applied, and a hunk whose lines disagree on the offset is refused. An edit
that leaves the file unchanged is an error, not a success.

| Parameter | Description |
|-----------|-------------|
| `path` (required) | File path |
| `diff` (required) | Unified diff |

### `shell`

Run a shell command and return stdout and stderr: `bash -c` on Unix, `cmd /C` on
Windows. Commands are non-interactive (stdin is `/dev/null`) and run in their
own session, so a timeout kills the whole process group. Output over about
20 KB keeps its first and last 100 lines. Exit code 1 is reported as output, not
as an error; other non-zero codes are errors. This is also how files are deleted
and directories listed.

| Parameter | Description |
|-----------|-------------|
| `shell_command` (required) | The command line |
| `description` | One line shown to you |
| `workdir` | Working directory; must stay inside the path scope (default: the scope root or the working directory) |
| `timeout_secs` | Default `tools.timeout_secs` (30), at most `tools.max_timeout_secs` (300); a larger request is cut to the ceiling. A command that runs out of time is killed and the result names the limit |

The path scope sets where a command starts, not what it may touch. A command
matching `tools.deny_patterns` is refused and never runs.

### `todo`

The session's task list; it lives in the session only.

| Parameter | Description |
|-----------|-------------|
| `action` | `read` (default), `add`, `update` or `set` (replaces the whole list; an empty list clears it) |
| `todos` | Items for `add`, `update` and `set`: `{id, content, status}`, status one of `pending`, `in_progress`, `completed`, `cancelled` |

### `tools`

Present only under deferred disclosure. Finds and loads the tools whose
definitions a request does not carry; see [Tool disclosure](../internals/tool-disclosure.md).

| Parameter | Description |
|-----------|-------------|
| `action` (required) | `search`, `load`, `list` or `help` |
| `query` | `search`: what you need, in plain words |
| `names` | `load`, `help`: tool names (at most 16 to load, 3 definitions shown for help) |
| `category` | `search`, `list`: `read`, `write`, `delete`, `shell`, `network`, `git`, `verify`, `device`, `agent` or `mcp` |
| `offset` | `list`: first entry (25 per page) |

### `attach_file`

Attach an image or audio file to the conversation. Needs the `media` feature.
Images are `png`, `jpg`/`jpeg`, `gif`, `webp`, `bmp` and `tiff`; audio is `wav`.
Audio goes to the model when it accepts audio; otherwise it is transcribed to
text through a speech-to-text model served over D-Bus (`tools.asr`, build
feature `asr`, Linux), and refused without that.

| Parameter | Description |
|-----------|-------------|
| `path` (required) | Image or audio file |
| `note` | Note recorded with the attachment |
| `force_transcribe` | Audio: transcribe even when the model accepts audio (offered only in builds with `asr` on Linux) |

### `web_fetch`

Fetch an http(s) URL and return it as readable text (HTML becomes markdown, JSON
is pretty-printed). Up to three redirects, 30 s timeout, no authentication, no
private-address filter.

| Parameter | Description |
|-----------|-------------|
| `url` (required) | http or https URL |
| `max_chars` | Truncate to this many characters (default `tools.web.fetch_max_chars`, 20000) |

### `web_search`

Search the web through the Brave Search API. Needs an API key in
`tools.web.search.api_key` or the `BRAVE_API_KEY` environment variable; without
one the call returns an error.

| Parameter | Description |
|-----------|-------------|
| `query` (required) | Search query |
| `count` | Results, 1 to 10 (default 5) |

### `task`

Delegate to a sub-agent. `spawn` (the default) runs the sub-agent to completion
and returns its final answer; the call blocks, so several calls run several
sub-agents. The other actions inspect a finished sub-agent's output buffer or
list the discovered subagent personas. A sub-agent runs as its own process with
the sub-agent tool set, cannot spawn further sub-agents, and is held to the
parent's permissions and approval mode. See [Delegation](../concepts/delegation.md).

| Parameter | Description |
|-----------|-------------|
| `action` | `spawn` (default), `status`, `read`, `grep`, `agents`; `ledger` with addressable context |
| `prompt` | `spawn`: complete, self-contained task (required for `spawn`) |
| `description` | `spawn`: short label shown in the TUI |
| `mode` | `spawn`: `research`, `plan`, `agent` (default) or the name of a discovered subagent persona |
| `workdir` | `spawn`: working directory, inside the session's scope |
| `model` | `spawn`: model override |
| `handle` | `status`, `read`, `grep`: output buffer handle from a spawn (required) |
| `start_line`, `end_line` | `read`: line range (default 1 to 50) |
| `pattern`, `context_lines`, `limit` | `grep`: regex (required), context (default 2), maximum matches (default 50) |
| `evidence`, `evidence_visibility`, `deliverable`, `paths` | `spawn`, only with addressable context (`context.assembly.mode` other than `off`): chunk ids to brief the child with, how much of them to show, the answer shape wanted, and the files it works on |

In a read-only session the children are held to read-only modes.

### `memory`

Key-value notes that persist across sessions, plus read access to the project's
knowledge documents (`.sven/knowledge/`). Notes live in `memory.json` in sven's
config directory unless `tools.memory.memory_file` says otherwise.

| Parameter | Description |
|-----------|-------------|
| `action` (required) | `set`, `get`, `delete`, `list`, `search_knowledge`, `list_knowledge` |
| `key`, `value` | `set` takes both; `get` and `delete` take `key` |
| `query` | `search_knowledge`: keywords |

`set` and `delete` are write calls; the other actions are read calls. In `plan`
and `research` the kernel refuses the write actions.

### `semantic_memory`

Facts, notes and contacts in the persistent SQLite memory store, recalled by
words. Present in builds with the `memory` feature when the store opens.

| Parameter | Description |
|-----------|-------------|
| `action` (required) | `remember`, `recall`, `forget`, `list`, `get` |
| `content`, `tags`, `entity`, `source` | `remember`: text, tags, who or what it is about, where it came from |
| `query` | `recall`: words to search for |
| `limit` | `recall` (default 10), `list` (default 20) |
| `tag_filter` | `list`: only memories with exactly this tag or entity |
| `id` | `forget`, `get`: memory id (opaque text) |

`remember` and `forget` are write calls; `recall`, `list` and `get` are read calls.
See [Memory](../concepts/memory.md).

### `skill`

Load, list or search skills, the task-specific instructions bundled with a
project. See [Skills](../guides/skills.md).

| Parameter | Description |
|-----------|-------------|
| `action` (required) | `load`, `list` or `search` |
| `name` | `load`: the skill's command, for example `sven/plan` |
| `regex` | `list`: filter on command, name or description; empty lists all |
| `query` | `search`: what the task needs, in plain words (up to 8 results) |

### `system`

Session controls. `switch_mode` moves to `research`, `plan` or `agent`, only
within the authority of the mode the session started in (a read-only session
cannot switch to a mode that can write). The MCP actions are refused in research
and sub-agent sessions, and in any session whose current mode is not `agent`.

| Parameter | Description |
|-----------|-------------|
| `action` (required) | `switch_mode`, `switch_model`, `add_mcp_server`, `remove_mcp_server` |
| `mode` | `switch_mode`: `research`, `plan` or `agent` |
| `model` | `switch_model`: fuzzy model name |
| `name` | MCP actions: server name, which becomes the tool prefix |
| `command`, `args`, `env` | `add_mcp_server` (stdio): executable, arguments, environment |
| `url` | `add_mcp_server` (http): server URL |

The MCP actions write the nearest `.sven/config.yaml`. As calls, `switch_mode`
and `switch_model` are read calls, `remove_mcp_server` is a write call and
`add_mcp_server` is a shell call, because it starts a command.

### `ask_question`

Ask you one to three multiple-choice questions and wait for the answers; "Other"
is always appended. Offered where the session can route a question: the
interactive TUI answers it, an embedding host may park it or answer it at once
with a "no user available" reply, and a session assembled without a question
channel does not have the tool.

| Parameter | Description |
|-----------|-------------|
| `questions` (required) | 1 to 3 items of `{prompt, options (at least 2), allow_multiple}` |

### `context`

Memory-mapped analysis of content too large for the prompt. Full profile only
(see above), except that a session with addressable context offers a `context`
tool that carries `expand` alone, so a shortened tool result is never a dead end.
See [Large content](../guides/large-content.md) and
[Context tools](../internals/rlm-context-tools.md).

| Parameter | Description |
|-----------|-------------|
| `action` (required) | `open`, `read`, `grep`, `query`, `reduce`; `expand` with addressable context |
| `path` | `open`: file or directory |
| `include_pattern`, `recursive` | `open`, directories: file-name wildcard filter, recurse (default `true`); accepted though not listed in the definition |
| `handle` | `read`, `grep`, `query`, `reduce`: handle from `open` or `query` |
| `start_line`, `end_line`, `file` | `read`: line range; `file` picks one file of a directory handle |
| `pattern`, `context_lines`, `limit` | `grep`: regex, context lines (default 2), maximum matches (default 50) |
| `prompt` | `query`, `reduce`: analysis or synthesis prompt |
| `ranges`, `chunk_lines`, `max_parallel` | `query`: explicit line ranges, lines per chunk, concurrent sub-queries |
| `chunk`, `first_line`, `last_line` | `expand`: chunk id (`c-` and 16 hex digits) and optional line range |

`open`, `read`, `grep` and `expand` are read calls. `query` and `reduce` send
content to the session's model and are network calls.

### `gdb`

Drive a GDB session against embedded hardware. Offered in the full tool set, on
Unix, in builds with the `gdb` feature. See [Debugging](../guides/debugging.md).

| Parameter | Description |
|-----------|-------------|
| `action` (required) | `start_server`, `connect`, `command`, `interrupt`, `wait_stopped`, `status`, `stop` |
| `server_command`, `force` | `start_server`: server command line (discovered from the project when omitted); kill whatever listens on the port first |
| `host`, `port`, `executable`, `gdb_path` | `connect`: server host (default `localhost`), port (inferred from `start_server`, else 2331), ELF for symbols, GDB binary (default `tools.gdb.gdb_path`) |
| `command` | `command`: the GDB command |
| `timeout_secs` | `command`, `interrupt`, `wait_stopped`: override the timeout |

### `android`

Control one attached Android device over ADB: `display_info`, `screenshot`, `tap`,
`swipe`, `type_text`, `key_event`, `go_home`, `launch_app`, `force_stop`,
`list_packages`, `current_app`, `wait`, `find_element`, `ui_signature` and
`screen_is_secure`. It is added only to sessions in the `ui-test` mode, in builds
with the `android` feature; `SVEN_ANDROID_SERIAL` pins the device when more than
one is attached, and the `serial` argument overrides it per call. It is gated as
device control, which no other mode grants.

## Core and deferred disclosure

`tools.disclosure.mode` is `deferred` for a program that reads the configuration
file and does not set it. A request then carries the full definitions of the
core tools and of the tools the model has loaded, and one summary line per other
tool in the system prompt. The core tools are `edit_file`, `find_file`, `grep`,
`read_file`, `shell`, `todo`, `write_file` and `tools`. The model loads the rest
with `tools` (`action: load`). `--tool-disclosure full` (or `mode: full`) sends
every definition with every request. Loading changes only what the model is
shown; a loaded tool is gated exactly as before. See
[Tool disclosure](../internals/tool-disclosure.md).

## Approval

Two things decide whether a call runs: the **mode**, which says what the
session may do at all, and the **approval mode**, which says whether an allowed
call is put to you first. Approval never widens a mode.

### What each mode allows

The kernel checks every call against the capability it is gated under:

| Mode | Allowed capabilities |
|------|----------------------|
| `agent`, `chat` | read, write, shell, network, git |
| `plan`, `research` | read, shell, network, git (never write) |
| `sdlc` | read everywhere; write and shell only in Execution, shell in Verification (see below) |
| `ui-test` | device control for the `android` tool |

### Default behaviour: `--approval auto`

`--approval auto` is the default, in every mode and on every surface. Every call
the mode allows runs without asking you, shell commands and file writes
included. Two things still apply:

- A shell command matching `tools.deny_patterns` is refused and never runs. The
  defaults are `rm -rf /*` and `dd if=*`. The same patterns refuse a `gdb` server
  command and the shell side of a GDB `shell`, `!` or `pipe` command, and they apply in
  `sven mcp serve`, in sub-agents and under manual approval.
- A call the mode does not allow fails without reaching the tool (for example
  `write_file` in `plan` or `research`).

### `--approval manual`

With `--approval manual` every call whose capability is not read-only is shown
to you with its tool and arguments and runs only after you answer `yes`. Read
calls (`read_file`, `grep`, `find_file`, `todo`, `skill`, `tools`, `task`, the
read actions of `memory`, `semantic_memory` and `context`) and sub-agent spawns
are not asked about; write, shell, network, git and device calls are,
including `web_fetch`, `web_search`, `write_file`, `edit_file`, `gdb`, and the
`query` and `reduce` actions of `context`. A sub-agent's and an SDLC task's calls come to the same prompt.
Manual approval needs the interactive TUI; sven refuses to start it headless, in
a pipe, as a subcommand or as a team member. Key bindings and the prompt are in
[Agents and modes](../concepts/agents.md#approval).

One exception: a `shell` command that matches `tools.auto_approve_patterns`, and
matches no deny pattern, runs without asking. The defaults are:

```yaml
tools:
  auto_approve_patterns: ["cat *", "ls *", "find *", "rg *", "grep *"]
  deny_patterns: ["rm -rf /*", "dd if=*"]
```

Patterns are globs (`*` any run of characters, `?` one character) matched against
the whole command text as written. A command that contains `;`, `|`, `&`, `<`, `>`,
a backtick, `$(` or a line break never matches an auto-approve pattern, so
`ls *` does not approve `ls; rm -rf ~`. Patterns are a convenience, not a security
boundary: confine what an agent may do with `--mode`, `tools.disabled` and a
sandbox. Setting either list replaces its defaults.

### Other surfaces

Over ACP and `sven mcp serve` a host-side permission requester can answer each
call. A tool then carries a default policy: `shell`, `write_file`, `edit_file`,
`task`, `gdb` and `android` ask; the rest run. `sven mcp serve` offers
`edit_file`, `find_file`, `grep`, `read_file`, `read_lints`, `shell`, `web_fetch`,
`web_search` and `write_file` (narrow it with `--tools`). `read_lints` (linter
diagnostics for files or directories, with `paths` and `workdir`) exists on that
surface only. `sven tool list` and `sven tool call <TOOL>` run the built-in tools
directly, without a model.

## Per-phase tools in `sdlc`

Each phase of the `sdlc` machine names its tools, in place of the mode listing:

| Phase | Tools |
|-------|-------|
| Intake, Discovery, Planning, Delivery, Recovery | `read_file`, `find_file`, `grep` |
| Execution | `read_file`, `find_file`, `grep`, `write_file`, `edit_file`, `shell` |
| Verification | `read_file`, `find_file`, `grep`, `shell` |

The kernel allows write only in Execution, shell in Execution and Verification,
and git operations in Discovery, Planning, Execution and Delivery.

## Tool definition size

Every definition is sent on every request, so their size is pinned by a test.
See [Tool definitions](../internals/tool-definitions.md).
