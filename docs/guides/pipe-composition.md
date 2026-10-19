# Pipe Composition

Sven follows the Unix philosophy: **pipes carry data, CLI arguments specify
operations**.  Every headless run writes structured text to stdout and
diagnostics to stderr, which keeps the stdout pipeline clean for downstream
tools or a second sven instance.

---

## When stdin is read

| Invocation | Stdin |
|------------|-------|
| `sven 'task'` | Not read, even when it is a pipe |
| `cmd \| sven --stdin 'task'` | Read to the end; joined with the prompt, a blank line between, into one user message |
| `cmd \| sven` | The task: read to the end |
| `sven --file workflow.md` | Not read, unless `--stdin` is given |

The decision depends on the arguments alone, so a run that already has its task
never stalls on an inherited pipe that nobody writes to or closes. When stdin is
the input and nothing arrives for two seconds, sven prints
`[sven:info] waiting for stdin ...` on stderr.

A run is headless, and so never opens the TUI, when stdin or stdout is not a
terminal; see [Headless and CI](headless.md#when-sven-runs-headless).

---

## Input format detection

When sven reads stdin it applies the following detection rules in priority
order:

| Priority | Detection criterion | Interpretation |
|----------|---------------------|----------------|
| 1 | Every non-empty line starts with `{` (the first 10 are checked) | **NDJSON trace steps** - one ATIF `TraceStep` JSON object per line, produced by `--output-format jsonl` |
| 2 | Any line is exactly `## User`, `## Sven`, `## Tool`, or `## Tool Result` | **Conversation markdown** - produced by `--output-format conversation` (default) |
| 3 | A single JSON object with a top-level `"steps"` array | **JSON trajectory** - the whole-document ATIF `Trajectory`, produced by `--output-format json` |
| 4 | Everything else | **Plain text** - treated as a single user message (one step). Workflow parsing (## steps, preamble) is **not** used for stdin; it only applies when using `-f`/`--file` with a workflow file. |

The rules are mutually exclusive and checked top-down. Priority 3 only ever
matches when priority 1 and 2 didn't: a pretty-printed `Trajectory` document
is multi-line JSON, so its second line doesn't start with `{` and the NDJSON
heuristic (priority 1) correctly returns false for it.

`--load-trace` and `--trace` read a file, not stdin, and always expect the third
shape: a single whole-document `Trajectory`, the same thing `--output-trace`
writes.

### Which runner reads which format

A headless run is served by one of two runners, chosen by its options and its
input (see [Single-prompt and workflow runs](headless.md#single-prompt-and-workflow-runs)).
The **single-prompt runner**, used by a plain `sven --stdin 'task'`, reads
plain text and conversation markdown. The **workflow runner** reads all four
formats. Piped NDJSON trace steps or a JSON trajectory select the workflow
runner on their own, so `sven 'task1' --output-format jsonl | sven --stdin
'task2'` seeds the second run with the first run's history.

---

## Output formats and what they produce downstream

| `--output-format` | stdout content | Piped into sven | Typical use |
|-------------------|---------------|-----------------|-------------|
| `conversation` (default) | Full `## User` / `## Sven` / `## Tool` / `## Tool Result` markdown, streamed | Detected as *conversation*, history seeded | Archiving, context passing, continuation |
| `compact` | The final response text of each step | Detected as *plain text*, becomes user message | Relay / transform chains |
| `json` | The full ATIF `Trajectory` document, pretty-printed, written at the end | Detected as *JSON trajectory*, history reconstructed from `steps` | CI dashboards, parsing with `jq`, feeding a fine-tuning pipeline |
| `jsonl` | One ATIF `TraceStep` JSON object per line, streamed as each step closes | Detected as *NDJSON trace steps*, history reconstructed from steps | Piping between sven instances with full fidelity (tool calls, thinking) |

Everything except `conversation` runs on the workflow runner.

> **Note on `json` vs `jsonl`**: `--output-format json` emits a single
> pretty-printed, multi-line `Trajectory` object (schema version, agent
> profile, and the whole `steps` array in one document) - the same shape
> `--output-trace`/`--trace` write to a file. `--output-format jsonl` emits
> one standalone `TraceStep` object per line as the run progresses, meant
> specifically for piping into a second sven instance. Both describe the same
> `TraceStep` schema; they differ only in whether it's one document or a
> line-delimited stream.

---

## Step content resolution for piped input

Stdin is **never** parsed as a workflow (no `##` step headings, no preamble).
Workflow structure applies only when you pass a workflow file with `-f`/`--file`.

When conversation markdown, NDJSON trace steps, or a JSON trajectory is piped
in, sven uses that format to seed history and resolve the new turn.  The step
content for the new turn is resolved in this order:

```
CLI positional prompt   →   piped pending user turn   →   hard error
```

1. **CLI positional prompt** (`sven 'task'`): the explicit task to run against the seeded history.
2. **Piped pending user turn**: a trailing `## User` section in conversation markdown, or a trailing `User`-source step with no following `Agent` step in NDJSON trace input, that has not yet received an agent response.  When present, it is used as the step content automatically. (A piped JSON trajectory carries no pending turn: it only seeds history and needs a prompt.)
3. **Neither present**: sven exits with code `2` and prints a diagnostic message explaining how to fix it.

---

## Pipe patterns

### Pattern 1 - Data transform (most idiomatic)

```bash
cat report.md | sven --stdin 'summarise the key findings'
find . -name '*.rs' | sven --stdin 'count lines in each file and sort by size'
git diff HEAD~1 | sven --stdin 'write a commit message for these changes'
```

The piped content is plain text (no `## User`/`## Sven` markers).  A
positional prompt makes stdin optional, so it is read only with `--stdin`;
sven then **combines them into a single user message**, the prompt and the
stdin text separated by a blank line.  So `cmd | sven --stdin "fix these
errors"` sends one message holding "fix these errors" and the command output.
Without `--stdin`, a prompted run never reads stdin, so an inherited pipe that
nobody writes to or closes (common under CI runners and tool harnesses) cannot
stall it.  Without a prompt, stdin is the task and is always read.  This
mirrors `grep`, `sed`, and `awk`: CLI arguments specify the operation, the pipe
carries the data.

### Pattern 2 - Context seed with explicit task

```bash
sven 'analyse the codebase and list all public APIs' \
  | sven --stdin 'write integration tests for each API listed above'
```

The first sven's conversation markdown is piped into the second.  The second
sven detects it as conversation format, seeds the first exchange into its
history, and runs `'write integration tests...'` as a fresh user turn with
that context available.

```
stdin (conversation markdown)
        │
        ▼
  parse_conversation()
        │
        ├─── .history ──────► seeded into the agent
        │
        └─── .pending_user_input (None here)
                                │
CLI prompt "write tests..."─────► step content
```

### Pattern 3 - Relay via pending user turn

A conversation file or output can end with an unanswered `## User` section.
When such output is piped to a second sven with no CLI prompt, the pending
user turn is automatically used as the step content:

```bash
# First sven produces a plan ending with:
#   ## User
#   Now implement step 1 of the plan.
sven --headless --file plan-and-relay.md | sven
```

Workflow file `plan-and-relay.md`:
```markdown
## Create a plan
Analyse the codebase and write a three-step improvement plan.
Output only the plan, then append exactly this line:
## User
Now implement step 1 of the plan.
```

This pattern lets one agent drive another without repeating the handoff
instruction on the CLI.

### Pattern 4 - Compact relay

`--output-format compact` emits only the agent's response text.  Because
there are no `## User`/`## Sven` markers, the receiving instance treats it
as a plain-text user message:

```bash
sven 'find all null-pointer dereferences in src/' --output-format compact \
  | sven --stdin 'fix each of the following bugs'
```

The second sven receives the bug list as its user message and runs from a
clean context (no seeded history).  This is the correct pattern when you
want the second agent to act on the *result* of the first, not have access
to how the first agent arrived at it.

### Pattern 5 - Full-fidelity ATIF trace chaining

For long pipelines where you want every agent in the chain to have access
to the complete history including tool calls and thinking blocks, persisted
as an ATIF trajectory document:

```bash
sven 'task1' --output-trace /tmp/run.atif.json
sven 'task2' --load-trace /tmp/run.atif.json
```

Or in a two-stage pipeline using a temporary file:

```bash
TMP=$(mktemp /tmp/sven-XXXX.atif.json)
sven 'stage 1' --output-trace "$TMP"
sven 'stage 2' --load-trace "$TMP"
```

The same history can travel on the pipe. Every non-empty line of
`--output-format jsonl` output is a standalone `TraceStep`; the receiving stage
(on the workflow runner, see [Which runner reads which format](#which-runner-reads-which-format))
seeds its history from it:

```bash
sven 'task1' --output-format jsonl \
  | sven --stdin 'task2' --output-format compact
```

> **Note**: `--output-trace`/`--load-trace`/`--trace` write and read a
> **file** containing one whole-document `Trajectory` object.
> `--output-format jsonl` streams one `TraceStep` object per line to
> **stdout**, for direct piping without an intermediate file. Both describe
> the same underlying `TraceStep` schema.

---

## `map`, `tee` and `reduce`

Three subcommands compose runs into fan-out and fan-in pipelines. Each spawns
`sven` child processes (the same binary) as `sven --headless --output-format F
[--model M] PROMPT` and captures their stdout; children's stderr is passed
through to yours. A child that exits non-zero fails the command, except exit
code `3` (tool errors), which is accepted.

```bash
# map: one agent per stdin line, {} is the line (4 at a time by default)
git diff --name-only HEAD~1 | sven map 'review {} for security issues'
cat files.txt | sven map --concurrency 8 'summarise {}'

# tee: the same stdin to N shell commands, in parallel
sven 'analyse the auth module' --output-format compact \
  | sven tee "sven --stdin 'find security issues'" \
             "sven --stdin 'find performance issues'"

# reduce: all of stdin plus a prompt into one synthesis agent
git diff --name-only HEAD~1 \
  | sven map 'review {} for security issues' \
  | sven reduce 'prioritise these findings and write a report'
```

`map` ignores empty lines, trims each line, writes the results in input order
and separates them with `---` (`--separator` changes it); a map child gets no
stdin. `tee` runs each argument with `sh -c` (`--shell` changes the shell),
writes the outputs in argument order with the same separator, and with no
command passes stdin through. `reduce` prepends `--preamble` to the collected
input. `map` and `reduce` default to `--output-format compact` and take
`-M/--model` (env `SVEN_MODEL`) for their children. The children read the
configuration file by the default lookup; a `--config` given to the parent is
not forwarded.

---

## Stderr stays clean

Sven always writes diagnostics to **stderr** so that stdout pipelines are
unaffected:

```bash
# Capture just the conversation on stdout; discard diagnostics
sven 'task' > result.md

# See only the progress lines
sven 'task' 2>&1 >/dev/null | grep '^\[sven:'

# Pass stdout downstream while monitoring stderr in the terminal
sven 'task' | sven --stdin 'follow-up'
```

Stderr lines use structured `[sven:tag]` prefixes:

| Prefix | Meaning |
|--------|---------|
| `[sven:settings]` | Workflow runner: the model and mode in effect |
| `[sven:step:start]`, `[sven:step:complete]` | Step beginning; step finished with timing, tool count and `success=` |
| `[sven:tool:call]`, `[sven:tool:result]` | Tool invocation (`id=`, `name=`, `args=`); result (`id=`, `name=`, `success=`, and `size=` or, for a failure, `output=`) |
| `[sven:tokens]` | Token usage for a model request, with running totals |
| `[sven:tokens:total]` | Single-prompt runner: total tokens of the run |
| `[sven:ledger]` | With `--token-ledger`: one JSON object per model request attributing its input tokens to their sources (see [token-ledger.md](../internals/token-ledger.md)) |
| `[sven:thinking]` | The model's reasoning text |
| `[sven:subagent:...]` | A `task` sub-agent started, ran a tool, finished or failed |
| `[sven:cache:hit]`, `[sven:cache:write]` | A step's `cache_key` was used or written |
| `[sven:trace]` | Where the trajectory was written |
| `[sven:dry-run]` | `--dry-run` report |
| `[sven:info]` | Informational (e.g. history loaded, `waiting for model N s (<phase>)` every 5 s while the model has produced nothing real, a changed served model) |
| `[sven:warn]` | Non-fatal warning (e.g. the served model does not advertise tool calling) |
| `[sven:error]`, `[sven:fatal]` | An error; the run normally exits right after |

---

## Error cases and exit codes

| Situation | Exit code | Stderr message |
|-----------|-----------|----------------|
| Conversation piped, no CLI prompt, no pending user turn | `2` | `[sven:error] Piped conversation has no pending task.` |
| NDJSON trace steps piped, no CLI prompt, no pending user turn | `2` | `[sven:error] Piped JSONL has no pending task.` |
| JSON trajectory piped, no CLI prompt | `2` | `[sven:error] Piped JSON summary has no pending task.` |
| Piped conversation or NDJSON fails to parse | warning; the input is not used as history | `[sven:warn] Failed to parse piped input as ...` |
| `--load-trace`/`--trace` file fails to load (malformed JSON) | `2` | `[sven:error] Failed to load --load-trace <path>: ...` |
| `--load-trace` file does not exist | `1` | `Error: --load-trace <path>: no such file` |
| `--trace` file does not exist yet | *(none - not an error)* | run proceeds with empty history; the file is created on first write |

The full list of exit codes of a run is in
[Headless and CI](headless.md#exit-codes). The error message for the "no
pending task" case also prints an example showing how to fix it:

```
[sven:error] Piped conversation has no pending task.

To continue a piped conversation provide a prompt:

    sven 'task1' | sven --stdin 'task2'

Or end the piped output with an unanswered ## User section
so the next sven instance picks it up automatically.
```

---

## Multi-stage pipeline example

```bash
#!/usr/bin/env bash
set -euo pipefail

# Stage 1: research (read-only, faster model)
sven 'Read src/ and list all exported public functions with their signatures' \
    --mode research \
    --output-format compact \
    --model anthropic/claude-haiku-4-5 \
    2>/dev/null \
  > /tmp/public-api.txt

# Stage 2: generate tests (stage 1's output is the data)
cat /tmp/public-api.txt \
  | sven --stdin 'Write a comprehensive test file for each function listed above' \
    --output-last-message tests/generated_tests.rs \
    2>/dev/null

# Stage 3: review, with stage 2's trajectory as history (the newest
# auto-logged trajectory; needs a .sven/ directory in the project)
sven --load-trace "$(ls -t .sven/logs/*.atif.json | head -1)" \
    'Review the generated tests for correctness and suggest improvements' \
    --output-format compact
```

---

## Relationship to `--resume` and `--trace`

| Flag | Use case |
|------|----------|
| `--resume ID` | Continue a saved TUI or CI conversation by ID (interactive or headless); a headless resume appends to the same session file |
| `--trace PATH` | Load + save an ATIF trajectory to the same file (read on start, rewrite on complete) |
| `--load-trace PATH` | Load trajectory history as context seed; does not write back |
| `--output-trace PATH` | Write the trajectory after run; does not read |
| Piped NDJSON trace steps (stdin) | Auto-detected by the workflow runner; seeds history same as `--load-trace` but from stdin, one `TraceStep` per line instead of a whole document |

Pipe-based NDJSON seeding is history-equivalent to `--load-trace` at runtime
(same `steps_to_messages` reconstruction); the wire shape and source differ
(line-delimited stdin vs. a whole-document file).

Sessions are stored as ATIF `.json` trajectories. A saved `.yaml` chat is
converted the first time it is opened, or all at once with
`sven migrate-sessions`.

---

## Implementation notes

The detection and routing live in `crates/ci/src/runner/` (`mod.rs`,
`helpers.rs`, `event.rs`) and `src/run/ci.rs`:

- `is_conversation_format(s)` - scans lines for reserved H2 headings.
- `is_jsonl_format(s)` - checks up to 10 non-empty lines for a `{` prefix (NDJSON trace steps).
- `is_json_summary_format(s)` - checks for a single JSON object with a top-level `"steps"` array (a `Trajectory` document).
- Detection order in the workflow runner: NDJSON trace steps → conversation → JSON trajectory → plain text (first match wins). Workflow parsing (## steps) runs only when input was read from a file (`-f`/`--file`); stdin is always one of the four shapes above.
- `parse_jsonl_trace_steps(s)` parses NDJSON into `(history, pending_user_input)` via `atif::persist::read_steps_ndjson` + `sven_session_store::trace_session::steps_to_messages`.
- `parse_json_summary(s)` parses a whole `Trajectory` document into a flat history via the same `steps_to_messages` reconstruction.
- Both return message history; step content = `extra_prompt` OR `pending_user_input` OR exit(2).
- The turn-shaped assembly (folding a flat event stream - user messages, assistant text, tool calls/results, thinking, context-compaction markers - into ATIF `TraceStep`s) (one agent step per model response: an assistant message that follows a tool result opens a new step) lives in `sven_session_store::trace_session::StepAssembler`, shared by every trace-producing/consuming path in the runner.

The unit tests in `crates/ci/src/tests.rs` cover the format detectors and the
conversation round trips.
