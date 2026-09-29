# Pipe Composition

Sven follows the Unix philosophy: **pipes carry data, CLI arguments specify
operations**.  Every headless run writes structured text to stdout and
diagnostics to stderr, which keeps the stdout pipeline clean for downstream
tools or a second sven instance.

---

## Input format detection

When stdin is not a terminal, sven reads it entirely and applies the following
detection rules in priority order:

| Priority | Detection criterion | Interpretation |
|----------|---------------------|----------------|
| 1 | Every non-empty line starts with `{` | **NDJSON trace steps** - one ATIF `TraceStep` JSON object per line, produced by `--output-format jsonl` |
| 2 | Any line is exactly `## User`, `## Sven`, `## Tool`, or `## Tool Result` | **Conversation markdown** - produced by `--output-format conversation` (default) |
| 3 | A single JSON object with a top-level `"steps"` array | **JSON trajectory** - the whole-document ATIF `Trajectory`, produced by `--output-format json` |
| 4 | Everything else | **Plain text** - treated as a single user message (one step). Workflow parsing (## steps, preamble) is **not** used for stdin; it only applies when using `-f`/`--file` with a workflow file. |

The rules are mutually exclusive and checked top-down. Priority 3 only ever
matches when priority 1 and 2 didn't: a pretty-printed `Trajectory` document
is multi-line JSON, so its second line doesn't start with `{` and the NDJSON
heuristic (priority 1) correctly returns false for it.

Note that this is about **piped stdin**, which is always either a stream of
individually-parseable lines or a single document. `--load-trace`/`--trace`
(a file path, not stdin) always expects the third shape - a single whole-
document `Trajectory` JSON file, the same thing `--output-trace` writes.

---

## Output formats and what they produce downstream

| `--output-format` | stdout content | Piped into sven | Typical use |
|-------------------|---------------|-----------------|-------------|
| `conversation` (default) | Full `## User` / `## Sven` / `## Tool` markdown | Detected as *conversation*, history seeded | Archiving, context passing, continuation |
| `compact` | Agent response text only | Detected as *plain text*, becomes user message | Relay / transform chains |
| `json` | The full ATIF `Trajectory` document, pretty-printed | Detected as *JSON trajectory*, history reconstructed from `steps` | CI dashboards, parsing with `jq`, feeding a fine-tuning pipeline |
| `jsonl` | One ATIF `TraceStep` JSON object per line, streamed as each step closes | Detected as *NDJSON trace steps*, history reconstructed from steps | Piping between sven instances with full fidelity (tool calls, thinking) |

> **Note on `json` vs `jsonl`**: `--output-format json` emits a single
> pretty-printed, multi-line `Trajectory` object (schema version, agent
> profile, and the whole `steps` array in one document) - the same shape
> `--output-trace`/`--trace` write to a file. `--output-format jsonl` emits
> one standalone `TraceStep` object per line as the run progresses, meant
> specifically for piping into a second sven instance. Both ultimately
> describe the same `TraceStep` schema; they differ only in whether it's one
> document or a line-delimited stream.

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
2. **Piped pending user turn**: a trailing `## User` section in conversation markdown, or a trailing `User`-source step with no following `Agent` step in NDJSON trace input, that has not yet received an agent response.  When present, it is used as the step content automatically. (A piped JSON trajectory does not carry this notion — it always seeds history only, same as today.)
3. **Neither present**: sven exits with code `2` and prints a diagnostic message explaining how to fix it.

This resolves the `sven | sven` bug where the second instance previously
sent an empty message to the model.

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
sven then **combines them into a single user message**: the prompt, a blank
line, then the stdin content.  So `cmd | sven --stdin "fix these errors"`
sends one message: "fix these errors" followed by the command output.  Without
`--stdin`, a prompted run never reads stdin, so an inherited pipe that nobody
writes to or closes (common under CI runners and tool harnesses) cannot stall
it.  Without a prompt, stdin is the task and is always read.  This mirrors `grep`, `sed`, and `awk`: CLI arguments
specify the operation, the pipe carries the data.

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
        ├─── .history ──────► agent.seed_history()
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
sven --file plan-and-relay.md | sven
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

Direct stdin NDJSON detection also works when the trace is streamed inline
rather than written to a file:

```bash
# Every non-empty line of the first sven's --output-format jsonl output is
# a standalone TraceStep JSON object, so the second instance auto-detects
# it and seeds history from it.
sven 'task1' --output-format jsonl | sven --stdin 'task2'
```

> **Note**: `--output-trace`/`--load-trace`/`--trace` write and read a
> **file** containing one whole-document `Trajectory` object.
> `--output-format jsonl` streams one `TraceStep` object per line to
> **stdout**, for direct piping without an intermediate file. Both describe
> the same underlying `TraceStep` schema.

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
sven 'task' 2>/dev/null | sven --stdin 'follow-up'
```

Stderr lines use structured `[sven:tag]` prefixes:

| Prefix | Meaning |
|--------|---------|
| `[sven:step:start]` | Step beginning |
| `[sven:step:complete]` | Step finished with timing and tool count |
| `[sven:tool:call]` | Tool invocation |
| `[sven:tool:result]` | Tool result (success or error) |
| `[sven:tokens]` | Token usage for the turn |
| `[sven:info]` | Informational (e.g. history loaded) |
| `[sven:warn]` | Non-fatal warning |
| `[sven:error]` | Fatal error before exit |

---

## Error cases and exit codes

| Situation | Exit code | Stderr message |
|-----------|-----------|----------------|
| Conversation piped, no CLI prompt, no pending user turn | `2` | `[sven:error] Piped conversation has no pending task.` |
| NDJSON trace steps piped, no CLI prompt, no pending user turn | `2` | `[sven:error] Piped JSONL has no pending task.` |
| Piped conversation fails to parse | warning + treat as plain text (single step) | `[sven:warn] Failed to parse piped input as conversation (...)` |
| Piped NDJSON trace steps fail to parse | warning + treat as plain text (single step) | `[sven:warn] Failed to parse piped input as JSONL trace steps (...)` |
| `--load-trace`/`--trace` file fails to load (malformed JSON) | `2` | `[sven:error] Failed to load --load-trace <path>: ...` |
| `--load-trace`/`--trace` file does not exist yet | *(none - not an error)* | run proceeds with empty history; the file is created on first write |

The error message for the "no pending task" case also prints an example
showing how to fix it:

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
    --output-format compact \
    --model anthropic/claude-haiku-4-5 \
    2>/dev/null \
  > /tmp/public-api.txt

# Stage 2: generate tests (uses stage 1 output as user message)
cat /tmp/public-api.txt \
  | sven --stdin 'Write a comprehensive test file for each function listed above' \
    --output-last-message tests/generated_tests.rs \
    2>/dev/null

# Stage 3: review (full conversation context from stage 2's auto-log trace)
sven --load-trace .sven/logs/$(ls -t .sven/logs/*.atif.json | head -1 | xargs basename) \
    'Review the generated tests for correctness and suggest improvements' \
    --output-format compact
```

---

## Relationship to `--resume` and `--trace`

| Flag | Use case |
|------|----------|
| `--resume ID` | Continue a saved TUI or CI conversation by ID (interactive or headless) |
| `--trace PATH` | Load + save an ATIF trajectory to the same file (read on start, rewrite on complete) |
| `--load-trace PATH` | Load trajectory history as context seed; does not write back |
| `--output-trace PATH` | Write the trajectory after run; does not read |
| Piped NDJSON trace steps (stdin) | Auto-detected; seeds history same as `--load-trace` but from stdin, one `TraceStep` per line instead of a whole document |

Pipe-based NDJSON seeding is history-equivalent to `--load-trace` at runtime
(same `steps_to_messages` reconstruction); the wire shape and source differ
(line-delimited stdin vs. a whole-document file).

There are no `--chat`-style flags: the legacy YAML `ChatDocument` format is
read-only (pre-existing `.yaml` sessions are imported when opened in the
TUI/GUI, and every save writes the ATIF `.json` format); it has no CLI I/O
path and is unaffected by anything in this document.

---

## Implementation notes

The detection and routing live in `crates/ci/src/runner/` (`mod.rs`,
`helpers.rs`, `event.rs`):

- `is_conversation_format(s)` - scans lines for reserved H2 headings.
- `is_jsonl_format(s)` - checks up to 10 non-empty lines for a `{` prefix (NDJSON trace steps).
- `is_json_summary_format(s)` - checks for a single JSON object with a top-level `"steps"` array (a `Trajectory` document).
- Detection order: NDJSON trace steps → conversation → JSON trajectory → plain text (first match wins). Workflow parsing (## steps) runs only when input was read from a file (`-f`/`--file`); stdin is always one of the four shapes above.
- `parse_jsonl_trace_steps(s)` parses NDJSON into `(history, pending_user_input)` via `atif::persist::read_steps_ndjson` + `sven_session_store::trace_session::steps_to_messages`.
- `parse_json_summary(s)` parses a whole `Trajectory` document into a flat history via the same `steps_to_messages` reconstruction.
- Both return message history; step content = `extra_prompt` OR `pending_user_input` OR exit(2). When stdin is plain text and a CLI prompt is given, the CLI merges them (prompt + blank line + stdin) before the runner sees input, so the runner gets one step.
- The turn-shaped assembly (folding a flat event stream — user messages, assistant text, tool calls/results, thinking, context-compaction markers — into ATIF `TraceStep`s) lives in `sven_session_store::trace_session::StepAssembler`, shared by every trace-producing/consuming path in the runner.

The unit tests in `crates/ci/src/tests.rs` cover every detection branch, the
priority chain, round-trips, tool-call preservation, thinking block handling,
and all documented error cases, against the current NDJSON/`TraceStep` wire
format.
