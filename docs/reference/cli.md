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
| `--goal-turns N` | off | Headless: take the whole prompt verbatim as the objective of a standing goal with `N` turns (1 to 500) and run it as a headless `/goal` does ([goals](../guides/goals.md)): exit 0 completed, 4 out of turns, 5 blocked, 6 paused. Quotes, line breaks and spacing are kept. Exits 2 for a prompt that is a `/goal` line, one over 4000 characters, or a workflow feature (`--file`, `--var`, a loaded trace, `--output-format`, ...); `--output-trace` writes its trajectory. |
| `--goal-check COMMAND` | none | Needs `--goal-turns`. The command line, run in the project after each turn, that decides the goal: it passes on exit status 0 (600 s limit). Quoted as `/goal --check` takes it. |
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
| `--orchestration MODE` | `agent.orchestration` | `off` or `plan`: whether the agent is offered `plan_tasks`; see [Orchestration](../concepts/orchestration.md). |
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
| `--record-requests FILE` | off | Append every model request this process sends (sub-agents included), exactly as sent, and a summary of what came back to FILE, one JSON object per line. `sven replay FILE` sends one again, optionally [with a prompt section or tool swapped](../guides/request-replay.md). |
| `-v, --verbose` | warn | `-v` debug logging and tool output snippets on stderr, `-vv` trace logging. `RUST_LOG` overrides the log filter. |

### Limits and workflow control

| Option | Default | Description |
|--------|---------|-------------|
| `--step-timeout SECS` | `agent.max_step_timeout_secs` (`0`) | Wall-clock limit of one workflow step; `0` is no limit. A `timeout=` directive on the step wins. |
| `--run-timeout SECS` | `agent.max_run_timeout_secs` (`14400`) | Wall-clock limit of the whole headless run, every wait included; `0` is no limit. A single-prompt run takes `--step-timeout` as its limit when `--run-timeout` is not given. The model is told the minutes used and left on every request. |
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
| `sven list-models [-p PROVIDER] [--json]` | Print every model that can be used now, with the name `--model` takes for it (`brain/Qwen/Qwen3-0.6B`, `openrouter/z-ai/glm-5.3-flash`), its context window, output cap and tool support: each local server that answers (brain, ollama, vLLM, LM Studio), each `providers:` entry and each hosted provider with a stored key, asked live. A source that does not answer or refuses its key is reported on stderr. `PROVIDER` is a driver id or a `providers:` entry; an unknown one is an error that lists them. sven bundles no model list: what it prints is what the endpoints serve. |
| `sven list-providers [-v] [--json]` | Print every provider id with its name and description; `-v` adds whether a key is stored and the base URL. Use the id as `model.provider`. |
| `sven auth set PROVIDER [--key-file FILE]` | Store a hosted provider's key in `~/.config/sven/credentials.yaml` (mode 0600), read from stdin (without echo on a terminal) or `FILE`, never from the command line. It serves every model of the provider and every `providers:` entry of that driver at the provider's own endpoint, never another `base_url`. Refused for `brain` (its key comes from its connection record) and for providers with no endpoint of their own (LiteLLM, Cloudflare, Azure), whose key is `api_key_file` beside the endpoint. |
| `sven auth list` | Print the providers with a stored key (never the keys). |
| `sven auth remove PROVIDER` | Forget a provider's stored key. |
| `sven auth login [--server URL] [--name NAME] [--no-browser]` | Sign this machine in to a remote-control server (`network` builds), `https://sven.swedishembedded.com` unless `--server` names another: opens the browser and prints the sign-in link on a line of its own, takes the one-time code back on a loopback port (authorization code with PKCE, RFC 7636 and 8252) and redeems it, checks the issued key and stores it in `~/.config/sven/remote.yaml` (mode 0600). `--no-browser`, and a session over SSH, instead print a link to open anywhere and read the code the page shows from the terminal. A server that predates the code grant gets the device grant (RFC 8628: an address and a short code to type). The server's models are then the `sven` provider's, and with no model configured sven starts with your account's default ([Providers](providers.md#the-sven-service-sven)). See [Remote control](../guides/remote-control.md). |
| `sven auth logout` | Forget the remote-control sign-in. |
| `sven doctor provider [--json]` | Check that the model sven would use can be reached: the provider builds, its endpoint answers and accepts the key, it serves the model, and the model can call tools. A hosted provider is asked for its model list only, which costs nothing. Honours `--model`. Exits `1` when the model cannot be used. |
| `sven doctor decisions [--json]` | Report what the decision runtime would do: selected model, readiness, supported question types, input limit, validation status, a latency profile and the inactive decision points with reasons. Contacts the provider even when `decision.enabled` is `false`. Exits `1` when decisions are unavailable. |
| `sven decisions models [--json]` | List the decision models the provider knows with state, validation, question types and input limit. Loads no model. |

### Sessions and trajectories

| Command | Description |
|---------|-------------|
| `sven chats [-n, --limit N]` | List saved sessions (default 20): ID to pass to `--resume`, update time, status and title. |
| `sven ledger FILE [--json]` | Summarise the [token ledger](../internals/token-ledger.md) of a saved trajectory, sub-agents included, as a table or one JSON object. |
| `sven replay FILE [--list] [--request N] [--repeat K] [--seed S \| --reported-seed] [--variant FILE] [--json]` | Send request N (default: the last) of a `--record-requests` log again, K times, to the model `-M`/`-c` select, and print each response (seed, input/cached/output tokens, tool calls, text) beside the recorded one. No tool runs, so the request just before a fault can be reproduced as often as needed while the server or drivers change. `--list` numbers the requests and shows the prompt sections (id and short hash) and the tool count each is made of. `--variant FILE` swaps or removes named prompt sections and tool definitions, or appends a message, before sending; `--json` prints one line per sample (variant, request, sample, tool calls with argument names and schema validity, text, output tokens, reasoning length). See [Replaying a request with a variant](../guides/request-replay.md). |
| `sven migrate-sessions [--dry-run]` | Convert every saved `.yaml` chat to an ATIF `.json` trajectory in one pass. Never overwrites an existing `.json` and never touches the `.yaml` files. Exits `1` if any conversion fails. |
| `sven validate -f FILE` | Parse a workflow file and print its title, frontmatter, preamble size and each step with its mode, provider, model and timeout. Exits `1` only when the file cannot be read; see [Workflows](../guides/workflows.md#validation-and-dry-run). |
| `sven oauth-callback URL` | Forward an `sven://` OAuth redirect to the local callback server (port `5598`, or `SVEN_OAUTH_CALLBACK_PORT`). Invoked by the OS protocol handler; call it by hand only to test. |

### Tools, index and verified tasks

| Command | Description |
|---------|-------------|
| `sven tool list` | List the built-in tools with their descriptions. |
| `sven tool call [TOOL] [key=value ...] [--json OBJECT]` | Without arguments, print every tool's parameter schema. `sven tool call TOOL` (or `TOOL --help`) prints that tool's schema. With arguments, run the tool: `key=value` pairs are strings, booleans (`true`/`false`) or integers; `--json '{"k":"v"}'` passes a raw object and overrides the pairs. See [Tools](tools.md). |
| `sven media generate --model ID --prompt TEXT --output FILE [--duration S] [--resolution R] [--aspect-ratio A] [--size WxH] [--seed N] [--generate-audio] [--action NAME] [--param KEY=VALUE]... [--reference KIND:PATH]... [--edit NAME --input-video FILE] [--json]` | Make a clip with a video model: exactly the request the [`run_model`](tools.md#run_model) tool makes, as a command a script or a harness check can run with no model turn. The provider is `tools.media.provider` (default: the sven service) with the key of its `providers.<name>` entry or `sven auth`; paths are inside the current directory, as for the tool. `--param` values are read as JSON when they are JSON (`steps=30`, `allow_community=true`) and are text otherwise; `--reference` is `image\|video\|audio:PATH`, repeated in the order the prompt counts them. The same request twice delivers the clip made the first time and submits nothing; a run killed while the server renders asks again under the same name, so a server that keeps its jobs (a running brain does, until it restarts) answers the job it already runs. Progress goes to stderr; stdout is the path written, or with `--json` one object: `path`, `digest` (SHA-256), `seconds`, `width`, `height`, `frames`, `reused`. Exit 1 with the reason when the request is refused before it is sent, the provider fails it, or it is not ready within `tools.media.timeout_secs`. |
| `sven media probe FILE [--seconds N [--tolerance T]] [--min-seconds N] [--max-seconds N] [--width PX] [--height PX] [--frames N] [--fps F] [--require-audio] [--json]` | Read an MP4 clip and check it is whole (every sample it lists is in the file): its length, size, codec, frames and sound. `--width`, `--height` and `--frames` must match exactly and `--fps` to within 0.01; a miss names expected against actual. `--json` prints the length, bytes, `video` (`codec`, `width`, `height`, `frames`, `fps`) and `audio`. Exit 1 when it is not a usable MP4 or misses a bound; a harness check uses it to refuse a cut-short, wrong-length or wrong-size shot. |
| `sven media join --output FILE [--list FILE\|-] [--dir DIR] [--video-only] [--json] [CLIP ...]` | Join MP4 clips, in order, into one file without re-encoding (the clips must be encoded alike), each clip's sound kept in sync with its picture. `--list` is a JSON array of paths or of objects whose `id` names `DIR/<id>.mp4`. See [`join_videos`](tools.md#join_videos). |
| `sven document convert IN.pdf OUT.md [--model SPEC] [--pages SEL] [--ocr auto\|always\|never] [--layout auto\|off\|regions] [--region-dpi N] [--layout-prompt TEXT] [--concurrency N] [--dpi N] [--max-tokens N] [--pages-per-request N] [--prompt TEXT] [--max-attempts N] [--page-markers] [--dedupe-headers] [--no-resume] [--json] [-q]` | Convert a PDF, or a picture of one page, to Markdown with an OCR model, resuming from the manifest beside OUT. Exit 2 when pages failed (pages that need OCR fail when no OCR model is available), 3 when no page could convert without one. See [Converting PDFs](../guides/document-conversion.md). |
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
| `harness work --login-file FILE --workspace DIR --scratch DIR` | Work one served harness run for the server that started this worker (`sven_control::work`): fetch the package and the run's files (each checked against its digest; the package is a serialized bundle or a tar archive of a package directory, below), run or resume, commit each publication with its checkpoint, pause or stop when the heartbeat says so, report the verdict. The login file is JSON (`run_url`, `token`, `generation`). Exit 0 finished, paused or cancelled; 3 when another worker owns the run (the server answered 409), after which nothing more is committed or reported. The worker is trusted and its agent is not, so the kernel keeps them apart (Linux Landlock, no option to turn it off): every command the agent's shell starts may read, write and execute in the workspace and a private tmp (`TMPDIR`), read and execute in the package's `bin` (a script finds its libraries there) and in the directories of the worker's `PATH` and the system's program, library and certificate directories, and reach nothing else - not the login file, the package's gate scripts and data, the scratchpad or the worker's scratch. It cannot connect to or bind a TCP port, and can signal and reach by abstract unix socket only its own processes. Gates, model calls, checkpoints and the run protocol run unconfined in the worker, whose memory is kept from its children. So do the agent's file tools, and a confined background process can rearrange the workspace while they run; they therefore open every path inside the kernel (`openat2` with `RESOLVE_BENEATH`, beneath the workspace held open as a directory), so no swap of a directory for a symlink can make `read_file`, `write_file` or `edit_file` touch anything outside the workspace (a relative link that stays inside still works, an absolute one is refused), and the worker reads the files it publishes the same way. The tools that hand a path to a walker, a library or another process cannot do that and are not offered: `find_file`, `grep`, `attach_file`, the document tools, `context` and `gdb` (the confined shell finds and searches). A kernel without Landlock ABI 6 (Linux 6.12 or newer) refuses to start the run, and `task` (delegation) is not offered, because a delegated child is an unconfined sven process. A command that starts its own session (`setsid`, a double fork) outlives its call, because the shell tool ends the command's process group; it stays inside the same Landlock domain. Ordinary `sven` runs are not confined. A package may bring toolkits as MCP servers ([Harnesses](harnesses.md#toolkits-as-mcp-servers); `sven harness run` starts them too, unconfined): `mcp.yaml` beside its main workflow names each (`servers: {<name>: {command, args, env, read_only, timeout_secs, tools}}`; `command` is a program in the package's `bin` or on `PATH`, `env` literal values only, `read_only` absolute directories it may read besides the system set, `/proc` included when the toolkit lists processes, and `tools` `allow` or `deny` per tool). Each server runs under the agent's confinement with a clean environment (`HOME` and `TMPDIR` in the private tmp) and no network; the run's agents get its tools by deferred disclosure, its older results shrink like command output, and a run whose server does not start and offer a tool fails before its first step. `make test/harness/toolkit` proves it with REA and Ghidra. When the descriptor's `models` names a `media_base_url` (an OpenRouter-compatible video API), the run's agents get `models` and `run_model`, generating with the run's token in the worker; without it they cannot generate video, whatever this machine is signed in to. |

A served run's package is either a serialized harness bundle (the default) or,
with `package_format: "tar"` in the descriptor, a plain tar archive of a package
directory. For a tar the descriptor also names `harness` (required: the main
workflow's path inside the archive; the workflows it imports are read from
beside it), and optionally `skills` (a directory of skills, copied to where the
agent finds skills in the workspace) and `bin` (a directory of tools, put first
on the `PATH` of the commands the run's shell and gate tools start; the
worker's own environment is not changed). The worker
unpacks the archive into the run's private package directory and makes it
read-only; the archive may hold regular files and directories only, so an
absolute path, a `..` component, a symlink, hard link or device refuses it, as
do more than 10,000 entries or 256 MiB of content. A server cannot know where
that directory is, so any input value that is exactly `{"package": "<path>"}`
is replaced by the absolute path of that entry of the archive (the run fails
when the path leaves the package or names nothing), and the `package_dir` input
is the archive's root. Example descriptor fields:
`"package_format": "tar", "harness": "package/harness.yaml", "skills": "package/skills", "bin": "package/bin", "inputs": {"printer": {"package": "package/printers/a1"}}`.

Run options (`run` and `resume`): `--input JSON` or `--input-file FILE`;
required: `--workspace DIR`, `--model SPEC`, `--model-endpoint URL` (both left
out by a harness whose plan asks no model) and
`--approvals auto|deny|ask`; optional: `--model-role ROLE=SPEC` (repeatable: the
model that answers a role the workflow names, on the same endpoint),
`--allow-capability NAME` (repeatable),
`--api-key-file FILE`, `--media-endpoint URL` and `--media-key-file FILE` (an
OpenRouter-compatible video API the run's agents generate with: `models` and
`run_model`), `--first-byte-timeout SECS`, `--timeout SECS`,
`--max-output-tokens N`, `--scratchpad DIR` (the run's private directory; its audit log goes there, never into the workspace), `--trace FILE` (JSONL), `--record FILE`,
`--trajectory FILE`, `--conversations DIR` (one ATIF file per conversation), `--checkpoint-out FILE`, `--json` (one JSON document on
stdout and nothing else), `--quiet` (no progress on stderr) and `--progress auto|json|text`
(`json` is one `[sven:harness] {...}` line per event, `text` a readable per-task log, and `auto`,
the default, is `text` on a terminal and `json` otherwise).
A run that receives SIGINT or SIGTERM suspends where it rests (exit 3) and, with
`--checkpoint-out`, leaves a checkpoint that `harness resume` continues.
Details: [Harnesses](harnesses.md).

### Servers and teams

These commands are in builds with the `network` feature (the default build).

| Command | Description |
|---------|-------------|
| `sven mcp serve [--tools NAMES] [--brave-api-key KEY]` | Serve sven's tools as an MCP server on stdin/stdout until the host disconnects. `--tools` is a comma-separated list of tool names; the default (also spelled `default`) is `edit_file`, `find_file`, `grep`, `read_file`, `read_lints`, `shell`, `web_fetch`, `web_search`, `write_file`. A name outside that set is an error; the set leaves out the tools that need a live session or change agent state. File tools and the shell's working directory are confined to the directory the server starts in, and the shell refuses commands matching `tools.deny_patterns`. The Brave key defaults to `BRAVE_API_KEY`. |
| `sven mcp add NAME URL [--header N=V]... [--oauth] [--client-id ID [--client-secret S]] [--local] [--scope S]`, `sven mcp add NAME [--env N=V]... [--scope S] -- COMMAND [ARGS...]` | Says where the server goes, then adds it. Signed in to sven, `NAME URL` alone goes to your account: the service's sign-in link is printed on a line of its own (the browser is opened too) and the command waits until the server connects. Otherwise, or with `--local`, it goes to a configuration file (`--scope`: `user`, the default, `project` or `workspace`); a server that needs an OAuth sign-in is signed in to at once (link printed, a pasted redirect address accepted) and the command prints `Connected NAME: N tools.` A sign-in record from an older release is refused with a hint to `sven auth login` again or use `--local`. A URL is a Streamable HTTP server, anything else a stdio command. Write a secret as `${VAR}`. See [MCP](mcp.md#what-add-does). |
| `sven mcp list` | The servers in `mcp_servers:` (on this machine), then your account's `connectors` with the services connected there; each with whether it connects, its tool count, or what it needs (a sign-in, or the error). |
| `sven mcp auth SERVER` | Sign in to an OAuth-protected MCP server: prints the sign-in page and opens it in a browser if there is one; when the browser cannot come back to this machine, paste the address it ends on. The sign-in is kept for every later run. |
| `sven mcp logout SERVER` | Forget the sign-in of an MCP server. |
| `sven acp serve [options]` | Serve sven as an ACP agent on stdin/stdout; see [ACP](acp.md) and [IDE integration](../guides/ide-integration.md). Options: `--model` (env `SVEN_MODEL`), `--provider` (env `SVEN_PROVIDER`), `--max-tool-rounds N`, `--max-output-tokens N`, `--wall-clock-secs SECS`, `--disable-tool NAME` (repeatable), `--permission-timeout-secs SECS` (at least 1, default 60), `--approval auto\|manual` (default `auto`), `--command-patterns JSON`, `--evidence-store DIR`. The budget options only lower what the configuration allows; the last three are set by a parent agent for the sub-agents it starts. |
| `sven rc [--working-dir DIR] [--wide-access] [--login-file PATH] [--hosted --sandbox DIR]` | Run sessions on this machine for the server `sven auth login` signed in to, working in `DIR` (default: the current directory), until Ctrl-C; it takes no input of its own. File tools stay in `DIR`, `web_fetch` reaches only the public internet and gdb runs no host program the model spells out, unless `--wide-access`. `--hosted --sandbox DIR` gives sessions only web search, web fetch and finding skills, and confines the whole process (Linux Landlock) to `DIR`, where it reads its sign-in and keeps everything. `--login-file PATH` (not with `--hosted`) signs in from a YAML file holding `server` and a single-use `start_credential` instead of a saved sign-in, keeping the token in memory only; SIGTERM stops the host like Ctrl-C. See [Remote control](../guides/remote-control.md). |
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
