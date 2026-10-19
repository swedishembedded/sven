# Agents and modes

A sven session runs in a **mode**. The mode decides which state machine drives
the session and which capabilities the kernel grants it. A separate setting, the
**approval** mode, decides whether the calls a mode permits wait for you. The two
are independent: the mode is a ceiling on what can happen, the approval mode only
adds questions inside that ceiling.

## Modes

There are five modes. Select one with `--mode` (`-m`), with `/mode` in the TUI, or
cycle with `F4`. The default is `agent`.

| Mode | Machine | File-write tools | Shell | Use it for |
|------|---------|------------------|-------|------------|
| `research` | reactive agent | refused | none (read-only tool set) | Reading and answering questions without touching anything |
| `plan` | reactive agent | refused | available | Producing a written plan before making changes |
| `agent` | reactive agent | allowed | available | General-purpose agentic work (**default**) |
| `chat` | reactive agent | allowed | available | Conversation; same permissions as `agent`, delegation limited to read-only sub-agents |
| `sdlc` | SDLC | Execution phase only | Execution and Verification phases only | Feature work with explicit phases and gates |

`research`, `plan` and `agent` run the same machine (the streaming,
native-tool-calling `ReactiveAgentMachine`); what separates them is what the kernel
permits and which tools the session is given. `chat` runs that machine too, under
its own registration and a conversational system prompt. `sdlc` runs the
`SdlcMachine`.

```sh
sven --mode research "What does the authentication module do?"
sven --mode plan "Design a rate-limiting layer for the API."
sven "Implement the rate-limiting layer described in the plan."   # agent
sven --mode sdlc "Add rate limiting to the API."
```

A positional prompt runs headless (see [Headless and CI](../guides/headless.md));
start the TUI with `sven` alone and pick the mode with `--mode`, `F4` or `/mode`.

### `research`

The session gets a read-only tool set: `read_file`, `find_file`, `grep`,
`web_fetch`, `web_search`, the memory and skill tools, `ask_question`, `todo`,
`system` and `task`. There is no `shell` tool. The kernel also refuses every
file-write capability, so a write the model proposes anyway (including a memory
write) fails without reaching the tool. `task` sub-agents are held to read-only
modes.

### `plan`

`plan` has the full coding tool set, but the kernel refuses the file-write
capability: `write_file`, `edit_file` and memory writes fail. The model is asked
for a numbered plan and to read, not modify. `shell` stays available, so `plan`
is a permission boundary on the file tools, not a sandbox: a shell command can
still change files. Use `research` when nothing may run. `task` sub-agents are
held to read-only modes.

### `agent`

Full coding tool set with read, write, shell, network and git capabilities
granted. This is the default mode.

### `chat`

`chat` is registered under its own name and prompts the model as a conversational
assistant. The kernel grants the same capabilities as `agent`. The difference that
is enforced is delegation: a `chat` session cannot start a `task` sub-agent with
more authority than read-only, and it cannot switch itself to `agent` or `sdlc`
with the `system` tool.

### `sdlc`

`sdlc` encodes the engineering lifecycle as phases, each running its own scoped
model and tool deliberation on an append-only thread and returning a structured
decision that drives the next transition:

```
Idle → Intake → Discovery → Planning → Execution → Verification → Delivery
                   │                                                  │
                   └──────────────── Recovery ◀──────────────────────┘
```

The kernel grants capabilities per phase:

| Phase | Capabilities |
|-------|--------------|
| Intake, Discovery, Planning, Delivery | read; Discovery, Planning and Delivery also git |
| Execution | read, write files, shell, git, start child runs |
| Verification | read, shell |
| Recovery | read |

No phase has network access, so `web_fetch` and `web_search` are refused in `sdlc`.

- **Intake** classifies the request. Chit-chat or under-specified requests stay in
  `Intake` and ask clarifying questions; only a confirmed, actionable scope
  advances.
- **Execution** decomposes the plan and fans out one concurrent child per task,
  then merges the child summaries back into the parent thread. See
  [Parallel submachine fan-out](../internals/parallel-submachines.md).
- **Recovery** is entered when a phase reports failure, and decides whether to
  retry or abort.
- A phase that needs your input asks you; a decision of `need_approval` is put to
  you only under `--approval manual` and is approved at once under `auto`.

The decision envelope, per-state tools and the thread invariant are described in
[Deliberation engine](deliberation.md).

### Other machines

`sven task run` drives the `verified-task` machine, and `sven agent step --mode`
takes any machine name this build registers (`agent`, `chat`, `sdlc`,
`verified-task`, `predict`; also `reactive` as an alias of the reactive agent
machine). `--mode` on the main command accepts only the five modes above.

## Switching modes

- `--mode <name>` (`-m`) sets the starting mode.
- `/mode <research|plan|agent|chat|sdlc>` stages a mode for the next message.
- `F4` cycles `research → plan → agent → chat → sdlc → research`.
- A new session started with `/new` starts in `agent`.

Switching rebuilds the session's machine and permissions for the next turn. The
approval mode set at start-up is kept.

## Approval

`--approval` takes `auto` (default) or `manual`.

### `--approval auto`

Every call the mode permits runs without a prompt, shell commands and file writes
included. A question the agent asks you (`ask_question`) is still put to you in
the TUI.

### `--approval manual`

Every call whose capability is not read-only is put to you before it runs. Reading
files, delegating to a `task` sub-agent and running a verifier are read-only;
file writes, shell, network access (`web_fetch`, `web_search`, MCP tools), git
operations and device control all ask. The calls of a `task` sub-agent and of an
SDLC task child come to the same prompt. Each call is asked about on its own:
approving one `shell` command does not approve the next.

Manual approval only adds prompts. It never grants what the mode refuses, so
`plan` still cannot write under `manual`.

The prompt is a modal naming the tool and its command, path or arguments, with
`yes` and `no`:

| Key | Action |
|-----|--------|
| `↑` / `↓`, `1`-`9` | Choose `yes` or `no` |
| `Enter` | Answer: `yes` runs the call, anything else refuses it |
| `Esc` | Dismiss: the call is refused |

A refused call is answered to the model with the refusal so it can take another
route.

Manual approval needs a person at a terminal. `sven` refuses to start with
`--approval manual` when the run is headless (`--headless`, a PROMPT, or stdin or
stdout not a terminal), when a subcommand is given, as a team member, or in
node-proxy mode.

### Shell command patterns

Two configuration lists classify shell commands (see
[Configuration](../reference/configuration.md)):

| Key | Default | Effect |
|-----|---------|--------|
| `tools.deny_patterns` | `rm -rf /*`, `dd if=*` | A matching command is refused in every mode and every approval setting |
| `tools.auto_approve_patterns` | `cat *`, `ls *`, `find *`, `rg *`, `grep *` | Under `manual`, a matching command runs without asking |

Patterns are globs matched against the whole command text; a list you configure
replaces the default list. Deny patterns are checked first. A command containing
`;`, `|`, `&`, `<`, `>`, a backtick, `$(` or a line break never matches an
auto-approve pattern, so `ls *` does not approve `ls; rm -rf ~`. Pattern matching
is a convenience, not a security boundary.

### Without a person

In headless runs, `sven agent step`, and other unattended runs, the approval mode
is `auto` and nobody is asked: every call the mode permits runs, and a question
the agent asks is answered at once with "No user is available to answer this
question. Proceed on your best judgement and state the assumption you made." Use
`--mode research` (no shell, no writes) when an unattended run must not change
anything, and `tools.disabled` to withhold individual tools.
