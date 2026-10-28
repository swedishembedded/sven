<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Command-line reference

```
sven [OPTIONS] [PROMPT]
sven [OPTIONS] <COMMAND>
```

`sven` with no subcommand runs one agent session. It opens the interactive
terminal UI when it is attached to a terminal, and runs [headless](#headless-detection)
otherwise. The options below belong to that session; they are written before a
subcommand, not after it (`sven --config ci.yaml doctor provider`).
`sven --help` and `sven <command> --help` print the same text.

Precedence for every setting an option and the configuration file both set:
the command-line option wins over the [configuration file](configuration.md).
A per-step directive in a [workflow file](../guides/workflows.md) wins over
both for that step and the steps after it.

## Session options

### Task and input

| Option | Default | Description |
|--------|---------|-------------|
| `PROMPT` | - | The task. Giving one makes the run headless. |
| `--stdin` | off | Read stdin to the end and join it to `PROMPT` with a blank line as one user message (`cmd \| sven --stdin "fix these errors"`). Without `--stdin`, a run that has a `PROMPT` never reads stdin. Without a `PROMPT`, a non-terminal stdin is the task and is always read. |
| `-f, --file FILE` | - | A markdown [workflow file](../guides/workflows.md) (`#` title, preamble, `##` steps), a saved conversation, or an ATIF trajectory (`.json` extension: loaded as `--load-trace`). Stdin is never parsed as a workflow. In the TUI the steps are queued for review. |
| `--attach PATH` | - | Attach an image or audio file to the first user turn. Repeatable. Audio is sent as audio when the model accepts it and transcribed otherwise. Needs no tool call, so it works with models that cannot call tools. |
| `--var KEY=VALUE` | - | Template variable, substituted as `{{KEY}}` in workflow steps. Repeatable. An argument without `=` is ignored with a warning. |
| `--resume [ID]` | - | Resume a saved conversation by ID, unique ID prefix or file path. Without an ID the TUI opens its session picker; headless runs require an ID. `sven chats` lists the IDs. A headless resume appends to the same session file. |

### Mode, model and approval

| Option | Default | Description |
|--------|---------|-------------|
| `-m, --mode MODE` | `agent.default_mode` (`agent`) | What the agent may do at all: `research` and `plan` never write; `agent` has the read and write tools; `chat` and `sdlc` select those machines. A per-step `mode=` directive in a workflow accepts `research`, `plan` and `agent`. |
| `-M, --model MODEL` | configuration | Model for this run: `provider/name`, a named `providers:` entry, a catalog model or a bare name. Also read from `SVEN_MODEL`. |
| `--approval MODE` | `auto` | `auto` runs every call the mode allows, shell and file writes included, without asking. `manual` shows each call that is not read-only and runs it only after you approve; a `task` sub-agent's and an SDLC task's calls come to the same prompt. `manual` needs the interactive TUI: sven refuses to start it with a subcommand, headless, in a pipe or as a team member. |
| `--tool-disclosure MODE` | `tools.disclosure.mode` | `full` or `deferred`: whether every request carries every tool definition or only the core tools' and those the model loaded. |
| `--skill-disclosure MODE` | `skills.disclosure` | `full` or `deferred`: how much of each skill the system prompt lists. |
| `--subagent-disclosure MODE` | `subagents.disclosure` | `full` or `deferred`: how much of each sub-agent the system prompt lists. |
| `--knowledge-disclosure MODE` | `knowledge.disclosure` | `full` or `deferred`: how much of each knowledge document the system prompt lists. |
| `--context-assembly MODE` | `context.assembly.mode` | `off`, `demote` or `reconstruct`; see [Addressable context](../internals/context-assembly.md). |
| `-c, --config FILE` | auto-discovery | A configuration file merged over every file found by the [normal lookup](configuration.md#config-file-location). |

Disclosure and context-assembly options override the configuration file for
one process. A deferred default applies to a program that reads a
configuration file; see [Configuration](configuration.md#toolsdisclosure).

### Headless output

| Option | Default | Description |
|--------|---------|-------------|
| `-H, --headless` | auto | Run without the TUI; see [Headless detection](#headless-detection). |
| `--output-format FMT` | `conversation` | `conversation`: `## User` / `## Sven` / `## Tool` / `## Tool Result` markdown, pipeable into another sven. `compact`: the final response of each step as plain text. `json`: the run's complete ATIF trajectory document, pretty-printed. `jsonl`: one ATIF step per line, streamed as each step closes, pipeable into another sven. |
| `-o, --output-last-message PATH` | - | Write the final agent response to a file after the run; parent directories are created. |
| `--artifacts-dir DIR` | - | Write the full conversation (`conversation.md`) and one file per step (`NN-<label>.md`) to this directory, created if needed. |
| `--output-trace PATH` | `.sven/logs/<timestamp>.atif.json` | Write the ATIF trajectory to this path. The default exists only when a `.sven/` directory is found in the working directory or an ancestor. Conflicts with `--trace`. |
| `--load-trace PATH` | - | Seed the conversation history from a saved ATIF trajectory. The file must exist. Conflicts with `--trace`. |
| `--trace PATH` | - | `--load-trace PATH` plus `--output-trace PATH`: the one session-persistence flag for headless and TUI runs. A missing file starts a new session and is created on the first write; the TUI rewrites it after every turn. |
| `--rerun-toolcalls` | off | Replay the tool calls recorded in the loaded trajectory with fresh results before the model sees them. Needs `--load-trace` or `--trace`. |
| `--token-ledger` | off | Print one `[sven:ledger]` JSON line per model request on stderr. The ledger is stored in the trajectory either way; `sven ledger FILE` summarises it. |
| `--record-requests FILE` | off | Append every model request this process sends (sub-agents included), exactly as sent, and a summary of what came back to FILE, one JSON object per line. `sven replay FILE` sends one again. |
| `-v, --verbose` | warn | `-v` debug logging and tool output snippets on stderr, `-vv` trace logging. `RUST_LOG` overrides the log filter. |

### Limits and workflow control

| Option | Default | Description |
|--------|---------|-------------|
| `--step-timeout SECS` | `agent.max_step_timeout_secs` (`0`) | Wall-clock limit of one workflow step; `0` is no limit. A `timeout=` directive on the step wins. |
| `--run-timeout SECS` | `agent.max_run_timeout_secs` (`14400`) | Wall-clock limit of the whole headless run, every wait included; `0` is no limit. A single-prompt run takes `--step-timeout` as its limit when `--run-timeout` is not given. |
| `--max-tokens N` | unlimited | Stop the run when input plus output tokens reach `N` (exit code `4`). |
| `--dry-run` | off | Parse the workflow, print the steps on stderr (`[sven:dry-run] ...`) and exit without calling the model. |

### System prompt and tools

| Option | Default | Description |
|--------|---------|-------------|
| `--system-prompt-file PATH` | - | Use the file's contents verbatim instead of the built-in system prompt. |
| `--append-system-prompt TEXT` | - | Append text to the system prompt; after the file's content when `--system-prompt-file` is also given. |
| `--no-system` | off | Drop sven's own system prompt (identity, guidelines, project, git and CI context, skills, sub-agents, knowledge). Text from `--system-prompt-file` or `--append-system-prompt` is still sent verbatim; with none, no system message is sent. |
| `--no-tools` | off | Send no tool schemas; a tool call the model makes anyway is refused. |
| `--bare` | off | `--no-system --no-tools`. |
| `--regen-system-prompt` | off | Accepted for compatibility; the system prompt is always regenerated when a trajectory is loaded, so it has no effect. |

### Interface

| Option | Default | Description |
|--------|---------|-------------|
| `--nvim` | off | Use the embedded Neovim chat view in the TUI (default build only; the plain ratatui view is the default). |
| `-V, --version` | | Print the version. |
| `-h, --help` | | Print help. |

Hidden options `--team-name`, `--team-role` and `--teammate-name` (alias
`--agent-name`) are set by `sven team start` when it starts a team member and
are not for direct use.

### Headless detection

A run is headless when any of these holds:

- `--headless` is given;
- there is a `PROMPT`;
- stdin is not a terminal (`echo task | sven`);
- stdout is not a terminal (`sven 'task' | sven --stdin 'follow-up'`).

Otherwise sven opens the TUI. `--file` alone does not make a run headless: in a
terminal it queues the steps in the TUI. A build without the `tui` feature
(`--features minimal`) refuses to start the TUI and asks for `--headless`, a
`PROMPT` or `-f`.

## Subcommands

Subcommands that take a model or tool configuration read it with the same file
lookup as a session, and honour a preceding `--config`. `mcp serve` and
`acp serve` read the default lookup only; `harness` reads no configuration.
Everything a subcommand prints on failure goes to stderr and the exit code is
`1` unless the table in [Exit codes](#exit-codes) says otherwise.

### Information and setup

| Command | Description |
|---------|-------------|
| `sven show-config` | Print the effective configuration as YAML: every layer merged, defaults filled in. |
| `sven init` | Write `.sven.yaml` in the current directory naming an OpenRouter GLM model and no key. Refused when the directory already has a configuration file. |
| `sven config get KEY [--scope SCOPE]` | Print the value of KEY (a dotted path, `model.name`) in force: as a file writes it, else the default; with `--scope project\|workspace\|user`, only what that file says (exit 1 when it says nothing). |
| `sven config set KEY [VALUE] [--stdin] [--scope SCOPE]` | Write KEY into a configuration file (default the project's, `./.sven.yaml`), creating it as needed; VALUE is read as YAML. `--stdin` reads the value from stdin, without echo on a terminal, so a secret never reaches the shell history. A key sven does not know, or a value of the wrong type, is refused and the file left as it was. |
| `sven completions SHELL` | Print a completion script for `bash`, `elvish`, `fish`, `powershell` or `zsh`. |
| `sven list-models [-p PROVIDER] [--catalog] [--json]` | Print every model that can be used now, with the name `--model` takes for it (`brain/Qwen/Qwen3-0.6B`, `openrouter/z-ai/glm-5.3-flash`), its context window, output cap and tool support: each local server that answers (brain, ollama, vLLM, LM Studio), each `providers:` entry and each hosted provider with a stored key, asked live. A source that does not answer or refuses its key is reported on stderr. `PROVIDER` is a driver id or a `providers:` entry; an unknown one is an error that lists them. `--catalog` prints the bundled catalog instead. |
| `sven list-providers [-v] [--json]` | Print every provider id with its name and description; `-v` adds whether a key is stored and the base URL. Use the id as `model.provider`. |
| `sven auth set PROVIDER [--key-file FILE]` | Store a hosted provider's key in `~/.config/sven/credentials.yaml` (mode 0600), read from stdin (without echo on a terminal) or `FILE`, never from the command line. It serves every model of the provider and every `providers:` entry of that driver at the provider's own endpoint, never another `base_url`. Refused for `brain` (its key comes from its connection record) and for providers with no endpoint of their own (LiteLLM, Cloudflare, Azure), whose key is `api_key_file` beside the endpoint. |
| `sven auth list` | Print the providers with a stored key (never the keys). |
| `sven auth remove PROVIDER` | Forget a provider's stored key. |
| `sven auth login [--server URL] [--name NAME]` | Sign this machine in to a remote-control server (`network` builds), `https://sven.swedishembedded.com` unless `--server` names another: prints an address and a one-time code, waits while you approve the connection in the browser (device authorization, RFC 8628), checks the issued key and stores it in `~/.config/sven/remote.yaml` (mode 0600). See [Remote control](../guides/remote-control.md). |
| `sven auth logout` | Forget the remote-control sign-in. |
| `sven doctor provider [--json]` | Check that the model sven would use can be reached: the provider builds, its endpoint answers and accepts the key, it serves the model, and the model can call tools. A hosted provider is asked for its model list only, which costs nothing. Honours `--model`. Exits `1` when the model cannot be used. |
| `sven doctor decisions [--json]` | Report what the decision runtime would do: selected model, readiness, supported question types, input limit, validation status, a latency profile and the inactive decision points with reasons. Contacts the provider even when `decision.enabled` is `false`. Exits `1` when decisions are unavailable. |
| `sven decisions models [--json]` | List the decision models the provider knows with state, validation, question types and input limit. Loads no model. |

### Sessions and trajectories

| Command | Description |
|---------|-------------|
| `sven chats [-n, --limit N]` | List saved sessions (default 20): ID to pass to `--resume`, update time, status and title. |
| `sven ledger FILE [--json]` | Summarise the [token ledger](../internals/token-ledger.md) of a saved trajectory, sub-agents included, as a table or one JSON object. |
| `sven replay FILE [--list] [--request N] [--repeat K] [--seed S \| --reported-seed] [--json]` | Send request N (default: the last) of a `--record-requests` log again, K times, to the model `-M`/`-c` select, and print each response (seed, input/cached/output tokens, tool calls, text) beside the recorded one. No tool runs, so the request just before a fault can be reproduced as often as needed while the server or drivers change. `--list` numbers the requests. |
| `sven migrate-sessions [--dry-run]` | Convert every saved `.yaml` chat to an ATIF `.json` trajectory in one pass. Never overwrites an existing `.json` and never touches the `.yaml` files. Exits `1` if any conversion fails. |
| `sven validate -f FILE` | Parse a workflow file and print its title, frontmatter, preamble size and each step with its mode, provider, model and timeout. Exits `1` only when the file cannot be read; see [Workflows](../guides/workflows.md#validation-and-dry-run). |
| `sven oauth-callback URL` | Forward an `sven://` OAuth redirect to the local callback server (port `5598`, or `SVEN_OAUTH_CALLBACK_PORT`). Invoked by the OS protocol handler; call it by hand only to test. |

### Tools, index and verified tasks

| Command | Description |
|---------|-------------|
| `sven tool list` | List the built-in tools with their descriptions. |
| `sven tool call [TOOL] [key=value ...] [--json OBJECT]` | Without arguments, print every tool's parameter schema. `sven tool call TOOL` (or `TOOL --help`) prints that tool's schema. With arguments, run the tool: `key=value` pairs are strings, booleans (`true`/`false`) or integers; `--json '{"k":"v"}'` passes a raw object and overrides the pairs. See [Tools](tools.md). |
| `sven document convert IN.pdf OUT.md [--model SPEC] [--pages SEL] [--ocr auto\|always\|never] [--layout auto\|off\|regions] [--region-dpi N] [--layout-prompt TEXT] [--concurrency N] [--dpi N] [--max-tokens N] [--prompt TEXT] [--max-attempts N] [--page-markers] [--dedupe-headers] [--no-resume] [--json] [-q]` | Convert a PDF to Markdown with an OCR model, resuming from the manifest beside OUT. Exit 2 when pages failed (pages that need OCR fail when no OCR model is available), 3 when no page could convert without one. See [Converting PDFs](../guides/document-conversion.md). |
| `sven document score REFERENCE HYPOTHESIS` | Character error rate of a text file against a reference, whitespace-normalised. |
| `sven index build [--quiet]` | Build the repository context index (file tree, public symbols, import graph) into `.sven/index/index.json` under the project root. |
| `sven index query QUERY [--limit N]` | Case-insensitive substring search over symbol names and signatures (default 20 results). |
| `sven index stats` | Print statistics of the current index. |
| `sven task run FILE [--project-root DIR] [--model M] [--timeout SECS]` | Attempt a `.task.toml` task whose completion a declarative verifier checks, not the model's claim. The file holds `id`, `prompt`, `verifier` and optional `max_attempts`. `--project-root` defaults to the working directory; `--timeout` bounds all attempts together. Exit code `0` when a verdict was recorded (pass or fail, which the trajectory's reward carries), `5` when a person must decide, `1` otherwise. |
| `sven questions list [--json]` | List parked questions with no recorded answer. A headless run answers its own questions at once; questions park only when an embedding host chooses to. |
| `sven questions answer ID ANSWER` | Record a person's answer durably (append-only, hash-chained). It does not resume the session that asked. |
| `sven agent step [--state FILE] [--mode MODE] [--role TEXT] MESSAGE` | Advance an agent by exactly one step and print its reply: the shell form of the [SDK](sdk.md). With `--state` the agent is loaded from the file if it exists and written back, so successive calls continue one conversation; an unreadable state file is an error. `--mode` (default `agent`) applies to a fresh agent only; `--role` becomes its system prompt. The step runs unattended with the coding toolset. |
| `sven agent-dispatch` | Read one JSON request `{"mode", "device", "params"}` from stdin and write one JSON reply `{"ok": true, "output": ...}` or `{"ok": false, "error": ...}` as the last stdout line. Exit `0` covers both replies; non-zero is a malformed request or an internal fault. The only mode is `ui-test`, in a build with the `android` feature (default build). |

### Pipe composition

| Command | Description |
|---------|-------------|
| `sven map TEMPLATE [--concurrency N] [-M MODEL] [--output-format FMT] [--separator S]` | Run one headless sven per non-empty stdin line, substituting the line for `{}` in `TEMPLATE`. At most `N` run at once (default `4`). Output is written in input order, each result separated by `---` (default separator `\n---\n`). Child format defaults to `compact`. |
| `sven tee COMMAND... [--shell SH] [--separator S]` | Send an identical copy of stdin to each shell command (run as `SH -c`, default `sh`) in parallel and write their outputs in argument order, separated as for `map`. |
| `sven reduce PROMPT [-M MODEL] [--output-format FMT] [--preamble TEXT]` | Pass all of stdin, with `PROMPT`, to one headless sven and print its answer. `--preamble` is prepended before the collected input. Child format defaults to `compact`. |

A child that exits non-zero fails `map`, `tee` or `reduce` (exit `1`), except
exit `3` (tool errors), which is treated as success. See
[Pipe composition](../guides/pipe-composition.md).

### Harnesses

`sven harness` validates, inspects, runs, resumes, replays, tests and compares
programmable harnesses. Only `run` and `resume` use a model, the server named by
`--model SPEC --model-endpoint URL` that you already run; no sven configuration
or environment is read.

| Command | Description |
|---------|-------------|
| `harness validate PATH... [--json]` | Compile harnesses and their imports without running them. `PATH` is a file or a directory holding `harness.yaml`. |
| `harness inspect PATH [--json \| --graph dot\|mermaid]` | Show the generated steps, capabilities, limits, digests and imports; or draw the plan as a graph (steps grouped by task). |
| `harness compile PATH [--format json\|yaml] [-o FILE]` | Print the canonical compiled plan (the lowered plan with its source map). |
| `harness explain PATH [--step ID] [--input JSON \| --input-file FILE] [--elide N]` | Explain a workflow task by task, or one generated step: origin, successors, predecessors. With inputs, also show each prompt, system message and gate command as a run with those inputs would send it. |
| `harness status PATH --checkpoint FILE [--json]` | Where a suspended run rests, its counters and completed steps, and whether `resume` would accept the checkpoint. Exit 1 when it would not. |
| `harness schema [PATH]` | Print the harness document JSON Schema, or the input/output contract of the harness at `PATH`. |
| `harness run PATH [run options]` | Run a harness once, on a real model with the coding tools. |
| `harness resume PATH --checkpoint FILE [run options]` | Resume a suspended run from the checkpoint `run --checkpoint-out` wrote. A checkpoint from another program is refused. |
| `harness replay PATH --recording FILE [--expect-trace FILE] [--json]` | Replay a recording offline; no model, tool or child starts. |

Run options (`run` and `resume`): `--input JSON` or `--input-file FILE`;
required: `--workspace DIR`, `--model SPEC`, `--model-endpoint URL` and
`--approvals auto|deny|ask`; optional: `--allow-capability NAME` (repeatable),
`--api-key-file FILE`, `--first-byte-timeout SECS`, `--timeout SECS`,
`--max-output-tokens N`, `--scratchpad DIR` (the run's private directory; its audit log goes there, never into the workspace), `--trace FILE` (JSONL), `--record FILE`,
`--trajectory FILE`, `--conversations DIR` (one ATIF file per conversation), `--checkpoint-out FILE`, `--json` (one JSON document on
stdout and nothing else) and `--quiet` (no `[sven:harness]` progress lines).
A run that receives SIGINT or SIGTERM suspends where it rests (exit 3) and, with
`--checkpoint-out`, leaves a checkpoint that `harness resume` continues.
Details: [Harnesses](harnesses.md).

### Servers and teams

These commands are in builds with the `network` feature (the default build).

| Command | Description |
|---------|-------------|
| `sven mcp serve [--tools NAMES] [--brave-api-key KEY]` | Serve sven's tools as an MCP server on stdin/stdout until the host disconnects. `--tools` is a comma-separated list of tool names; the default (also spelled `default`) is `edit_file`, `find_file`, `grep`, `read_file`, `read_lints`, `shell`, `web_fetch`, `web_search`, `write_file`. A name outside that set is an error; the set leaves out the tools that need a live session or change agent state. File tools and the shell's working directory are confined to the directory the server starts in, and the shell refuses commands matching `tools.deny_patterns`. The Brave key defaults to `BRAVE_API_KEY`. |
| `sven acp serve [options]` | Serve sven as an ACP agent on stdin/stdout; see [ACP](acp.md) and [IDE integration](../guides/ide-integration.md). Options: `--model` (env `SVEN_MODEL`), `--provider` (env `SVEN_PROVIDER`), `--max-tool-rounds N`, `--max-output-tokens N`, `--wall-clock-secs SECS`, `--disable-tool NAME` (repeatable), `--permission-timeout-secs SECS` (at least 1, default 60), `--approval auto\|manual` (default `auto`), `--command-patterns JSON`, `--evidence-store DIR`. The budget options only lower what the configuration allows; the last three are set by a parent agent for the sub-agents it starts. |
| `sven rc [--working-dir DIR] [--hosted --sandbox DIR]` | Run sessions on this machine for the server `sven auth login` signed in to, working in `DIR` (default: the current directory), until Ctrl-C; it takes no input of its own. `--hosted --sandbox DIR` gives sessions only web search, web fetch and finding skills, and confines the whole process (Linux Landlock) to `DIR`, where it reads its sign-in and keeps everything. See [Remote control](../guides/remote-control.md). |
| `sven rc show ID [--follow]` | Print the finished messages of conversation `ID`, one that runs on this machine, as the server keeps it; with `--follow`, keep printing what is added from anywhere until Ctrl-C. |
| `sven rc attach ID [--tui]` | Print conversation `ID`, one that runs on this machine, as it changes, and send each line typed as its next message through the server; Ctrl-C or end of input detaches. `--tui` shows it in the full-screen UI instead. |
| `sven team list` | List teams. |
| `sven team status NAME` | Print a team's detailed status. |
| `sven team create --name N [--goal TEXT] [--max-active N] [--token-budget N]` | Create the persistent team directory under `~/.config/sven/teams/`. Defaults: `--max-active 8`, `--token-budget 0` (unlimited). |
| `sven team start -f FILE [--sven-bin PATH] [--dry-run]` | Spawn one sven process per member of a team definition YAML. `--dry-run` prints the commands. |
| `sven team cleanup NAME [--force]` | Remove a team's directory. |
| `sven team definitions` | List `.sven/teams/*.yaml` in the project. |
| `sven team init --name N [--goal TEXT]` | Write a starter `.sven/teams/<N>.yaml`. |
| `sven team watch NAME [--interval SECS] [--timeout SECS]` | Follow a team's status and events. Defaults: refresh every `3` s, run until interrupted (`--timeout 0`). |

See [Teams](../guides/teams.md).

## Exit codes

A headless run exits with:

| Code | Meaning |
|------|---------|
| `0` | The run finished. |
| `1` | Agent error: a step failed, a model or provider error, 20 consecutive tool errors, or an error before the run started (unreadable file, `--load-trace` file missing, unknown provider). |
| `2` | Invalid input: unreadable `--system-prompt-file`, malformed `--load-trace` file, or a piped conversation with no pending task. |
| `3` | A workflow run (`--file`) finished, but at least one tool call returned an error. A single-prompt run records tool errors in its trajectory and exits `0`, whichever runner serves it. |
| `4` | The `--max-tokens` budget was reached. |
| `5` | A person must decide: a verified task is parked, or a question was parked in a single-prompt run. The trajectory is left unscored so it can be resumed. |
| `6` | Stopped without concluding: the last model turn ended on text announcing a call it did not make. |
| `124` | A step or run timeout elapsed. |
| `130` | Interrupted with Ctrl+C. The partial trajectory is saved. |

`harness` commands use their own codes: `run` and `resume` exit `0` returned,
`1` the workflow failed, `2` usage or invalid harness, input or file, `3` suspended
by a bound, `4` cancelled, `5` infrastructure; `validate` and `inspect` exit
`0` valid, `1` invalid, `2` unreadable path; `replay` exits `0` matched, `1` trace mismatch, `2` refused recording.

## Environment

| Variable | Used by |
|----------|---------|
| `SVEN_MODEL` | Default for `--model` / `-M` of the session, `map`, `reduce` and `acp serve`. |
| `SVEN_PROVIDER` | Default for `sven acp serve --provider`. |
| `BRAVE_API_KEY` | Default for `sven mcp serve --brave-api-key` and the `web_search` tool. |
| `SVEN_OAUTH_CALLBACK_PORT` | Port `sven oauth-callback` forwards to (default `5598`). |
| `SVEN_LOG_FILE` | Append logs to this file in the TUI and in sub-agent processes, where logging to stderr is otherwise off. |
| `RUST_LOG` | Log filter; replaces the level chosen by `-v`. |

Provider keys are never read from the environment (`sven auth set`); a
running brain server is found from its connection record under
`$XDG_RUNTIME_DIR/brain`. `${VAR}` expansion in configuration files is
described in [Configuration](configuration.md).
