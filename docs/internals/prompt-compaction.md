# Prompt Compaction

Long-running agent sessions accumulate conversation history that eventually
exceeds the model's context window. This document explains how sven prevents
that from crashing workflows, what mechanisms fire and in what order, and how
to reason about the configuration knobs.

---

## The problem

Every LLM API enforces a hard ceiling on the number of tokens it will accept
in a single request. That ceiling is the **context window** - typically
expressed as a total of input + output tokens. When a request exceeds it, the
API returns a 400 error and the workflow fails.

In practice the ceiling is tighter than it looks:

- The model needs headroom for its reply, so the usable input is less than
  the full window.
- Tool schemas are sent with every request but are not stored in the
  conversation thread, so they consume budget invisibly.
- A dynamic system-prompt suffix (git branch, CI notes) is added per request
  for the same reason.
- The standard `chars / 4` token estimate is a rough approximation. Code files
  with many short identifiers are denser than prose; some providers tokenize
  differently.

Sven addresses these with a per-turn budget check, proactive compaction, a
hard request-size gate, and per-result truncation of tool output.

---

## Architecture overview

Compaction lives in the kernel's turn executor (`TurnExecutor`,
`crates/executors/src/turn.rs` and `turn/compaction.rs`, `turn/limits.rs`);
the pure building blocks live in `sven-turn` (`crates/turn/src/fork.rs`,
`compact.rs`) and `sven_model::budget`.

```
CallLlm { kind: "turn" } effect          (every model request, tool loop included)
  │
  ▼
TurnExecutor: snapshot the thread, resolve tool schemas
  │
  ▼
request size = server's count of the last request + estimate of what was appended
  │
  ▼
size ≥ compaction limit?                 ← only when the window is known
  ├─ no  → continue
  └─ yes → compact_thread()
             ├─ plan_fork(): the conversation (or its largest oldest prefix
             │     that fits) + the compaction prompt
             ├─ notes (thinking off; retried once with the provider default)
             │     └─ apply_summary(); repeat for the rest if only a prefix fit
             └─ no notes at all → emergency_compact(), loudly
  │
  ▼
estimate > input_budget?  → fail the turn with an actionable error (hard gate)
  │
  ▼
stream_turn()                 ← model call
  │     └─ response out of output budget while thinking, no answer
  │            → warn with the tokens used, compact, ask again once
  ▼
ToolExecutor (per tool call, concurrently)
  └─ smart_truncate()         ← cap each result before it enters the thread
```

---

## Layer 1 - Token accounting (`sven_model::budget`)

### The input budget

`effective_input_budget(context_window, max_output_tokens)` returns the usable
input ceiling: the model's context window minus a minimal output reserve
(256 tokens). The output limit actually requested for a call is computed
separately by `dynamic_output_budget`, scaled to whatever room the real prompt
leaves. For a model on a local server with no configured output cap that is
the whole of that room: thinking is part of the response and the server
enforces the bound.

The window comes from the provider: the smaller of the configured
`model.max_tokens` and the figure a local server reports (see
[Local model discovery](../reference/configuration.md#local-model-discovery)),
else the bundled catalog's entry for the model. When it is not known at all (a
hosted provider with no catalog entry and no live probe result), the budget is
`None` and both the compaction trigger and the hard gate are skipped rather
than guessing.

### The request size

The server's token count of the thread's previous request (prompt, cache reads
and writes) is the base. Only what was appended since - the reply, tool
results, the user's next message - is estimated, as `chars / 4 × 1.1`. No scale
factor is applied to the server's count, so an over-counting heuristic cannot
make compaction fire early; a burst of tool results costs only its own
estimation error. The count is used only while the messages it described still
lead the thread; a rewritten or compacted thread starts from the plain
estimate (`chars / 4` over messages, tool schemas and the dynamic suffix,
×1.1) until the next request reports.

---

## Layer 2 - Proactive compaction

### The trigger

Before every model request:

```
reply_cap    = min(compaction_summary_tokens, window / 4)
by_threshold = input_budget × max(compaction_threshold − compaction_overhead_reserve, 0)
               − window × compaction_output_reserve
by_fit       = window − prompt_tokens − reply_cap − 256
compact when   request size ≥ min(by_threshold, by_fit)
```

`by_fit` guarantees the request that asks for the notes - conversation, the
compaction prompt and room for the notes - fits the window. `by_threshold`
keeps `compaction_output_reserve` of the window free for the response, so a
normal turn always has room to think and answer. A thread the server has
not yet counted (a first request, or the request after a compaction) is also
compacted when its response-sizing estimate - the plain estimate raised by half,
since chars/4 undercounts tool schemas and the chat template - passes the input
budget, even under the limit above; otherwise the request would be refused for
want of room instead of compacted. A threshold of `1.0` turns all of this off.
At the start of a session
(and when it changes) sven prints the numbers in one `context budget:` line.

### The fork

`plan_fork` builds the request: the **real message list** (same system prompt,
same tool calls and results; it is not serialised into text) followed by one
user message, `COMPACTION_PROMPT`, which asks for everything needed to
continue: goal and assignment, constraints and rules, decisions and failed
approaches, files read or changed with key facts, commands and outcomes, exact
identifiers and values copied verbatim, current state, open questions and next
steps. The session's tool definitions are offered too, with `tool_choice:
none`, so that the fork shares its prompt prefix (and so a provider's prompt
cache) with the requests that use them; a provider that cannot forbid tool
calls is sent the fork without tools. The reply is capped at `reply_cap` tokens and is
requested with reasoning switched off (`enable_thinking: false`), so the
budget cannot be spent thinking, and the thinking-token cap does not apply to
it.

`apply_summary` rebuilds the history as
`[system, user(task), user(notes + todo list), ...rest]`. The task is the first
user message, kept verbatim, as is the todo list (the `todo` tool's state), so
neither is ever left to the summariser. A user message at the end of the
thread that no one has answered yet is not summarised: it stays after the
notes. The notes are a user message, so the next turn of a tool loop answers
them by carrying on.

A cut never falls inside a tool call or between a call and its results. A
model that answers with a batch of parallel tool calls leaves them stored as
every call and then every result, so the batch is one unit for a cut; when a
batch is larger than a fork can hold, the fork takes as many of its calls, with
their results, as fit, and the rest of the batch stays in the thread as a batch
of its own.

### Pieces

When the conversation does not fit one fork (a huge tool result, a long run
that outgrew the window), the fork covers the largest oldest prefix that fits
and ends at a clean boundary. Its notes replace that prefix, and the loop
repeats on what is left until the thread is back under the limit (at most six
rounds). Nothing is dropped. If even the oldest exchange alone does not fit,
its contents are clipped for the fork and the tokens cut are reported in a
warning. The first message (the task) is clipped on its own, to a third of the
fork's room: the task is kept whole outside the notes, so a large task does not
keep the rest of the conversation from being summarised.

### Retry

A failed call or empty notes is tried once more with the provider's default
reasoning setting.

### Output exhaustion

If a response runs out of its output budget while thinking and produced no
text or tool call, the executor warns with the thinking tokens spent, compacts
the thread and asks again once (the failed attempt's reasoning is not part of
the thread). A second exhaustion in a row is not retried.

### Rejected by the server

If the server rejects a request as too long for its context
(`context_length_exceeded`, `n_ctx`, ...), the estimate undercounted the
prompt. When the rejection says the response budget alone overshot (the prompt
fitting), the budget is lowered by the overshoot and the request is sent again,
dropping nothing. Otherwise the executor warns and compacts the thread with the
fork, calibrated by the server's own "needs N tokens" figure (the ratio of what
it counted to what sven estimated), and asks again once. A fork the server in
turn rejects is planned again, smaller, from the figure it gives. Only when no
window is known, so no fork can be planned, does the request fall back to the
model-free trim.

### Emergency path

`emergency_compact` is the last resort: it runs only when the model could not
produce notes at all, or a partial round of notes was no smaller than what it
replaced.

- Drop all non-system messages except the last six (adjusted to a clean
  tool-group boundary).
- Prepend a canned assistant notice telling the model that earlier history was
  dropped (with the todo list verbatim), followed by the first user message
  (the task) unless it is already in the kept tail.
- No model call - always succeeds regardless of session size.
- Reported as `CompactionStrategyUsed::Emergency`, with a warning that gives
  the number of tokens dropped.

### The hard gate

After compaction, if the estimate still exceeds the input budget, the turn
fails immediately with an error naming the estimate, the context window, the
provider and the model, instead of sending a request the server would reject.

---

## Layer 3 - Smart tool-result truncation

Large tool outputs are the primary cause of sudden context spikes. Before a
tool result is appended to the conversation thread, `ToolExecutor` passes it
through `sven_turn::smart_truncate`, which applies a content-aware extraction
based on the tool's declared `OutputCategory`. The untruncated output still
reaches `UiEvent::ToolCallFinished` and the audit trail.

### OutputCategory

Each tool in the `Tool` trait declares its output category via
`fn output_category(&self) -> OutputCategory`. The default is `Generic`.
The executor dispatches on the category - it never references tool names
directly.

| Category | Tools | Strategy |
|---|---|---|
| `HeadTail` | `shell`, `gdb` | Keep first 60 + last 40 lines; both the command preamble and the final result remain visible |
| `MatchList` | `grep`, `read_lints`, `memory`, knowledge and context search | Keep leading matches only; later matches are less relevant |
| `FileContent` | `read_file`, context and buffer reads | Balanced head + tail split; preserves imports/declarations and the most recent changes |
| `Generic` | all others | Hard-truncate at the nearest line boundary |

Every truncated result carries an explicit notice saying what was omitted,
for example:

```
[... 42 lines / 18340 bytes omitted ...]                                            (HeadTail)
[... 42 more matches omitted (18340 bytes); use a more specific pattern to see them ...]
[... 42 lines omitted (18340 bytes); use read_file with offset/limit to see more ...]
[... 18340 bytes omitted; content truncated to fit context budget ...]              (Generic)
```

The token cap is controlled by `tool_result_token_cap` (default 4000 tokens;
`0` disables truncation). The cap uses the same `chars / 4` approximation as
`approx_tokens`, so a token-dense code file might be allowed slightly more
than 4000 tokens after truncation; the budget check before the next model
call catches any remaining excess.

### Why separation of concerns matters here

By putting `output_category()` on the `Tool` trait rather than in
`compact.rs`, sven keeps crates independent:

- Each tool crate owns *what shape* each tool's output has.
- `sven-turn` owns *how to truncate* each shape; `sven-executors` applies it.
- Adding a new tool never requires editing either. The tool just overrides
  `output_category()`.

---

## Split safety for tool-call/result pairs

Anthropic (and other providers) require that every `tool_result` block in the
conversation has a corresponding `tool_use` block in the preceding assistant
message. A fork covering a prefix therefore only ends where the next message
is not a `ToolResult` and the last covered message is not a `ToolCall`, and
the emergency path starts its kept tail the same way (`clean_boundary_start`).

---

## Event reporting

Every compaction emits `UiEvent::ContextCompacted` (`AgentEvent` is the same
type):

```rust
ContextCompacted {
    tokens_before: usize,             // history estimate before compaction
    tokens_after:  usize,             // history estimate after rebuild
    strategy: CompactionStrategyUsed, // Fork | Emergency (Structured and Narrative: older sessions)
    turn: u32,                        // 0 for compaction before a model turn
}
```

The TUI displays this in the chat pane. CI logs it to stderr as:

```
[sven:compact] strategy=fork turn=0 60674->7144
```

---

## Configuration reference

All fields live under the `agent` section of the sven configuration (see
[Configuration](../reference/configuration.md#config-file-location) for the
files that are read).

| Key | Default | Description |
|---|---|---|
| `compaction_threshold` | `0.85` | Fraction of the input budget that triggers compaction |
| `compaction_output_reserve` | `0.25` | Fraction of the window kept free for the response, reasoning included |
| `compaction_summary_tokens` | `10000` | Most tokens the notes may take (also at most a quarter of the window) |
| `compaction_overhead_reserve` | `0.0` | Fraction of the input budget set aside for what the token count does not see |
| `tool_result_token_cap` | `4000` | Per-result token ceiling before truncation (`0` disables) |

`compaction_keep_recent` and `compaction_strategy` are retired: they are
accepted and reported as ignored.

### Tuning for CI / long-running workflows

```yaml
agent:
  compaction_threshold: 0.75       # fire earlier, more margin
  tool_result_token_cap: 2000      # tighter cap if build logs are large
```

### Tuning for interactive sessions

```yaml
agent:
  compaction_summary_tokens: 16000 # more detail in the notes
  tool_result_token_cap: 8000      # more context per file read
```

---

## Compaction failure modes and mitigations

| Failure | What sven does |
|---|---|
| Notes call fails or returns empty text | Asks once more with the provider's default reasoning setting |
| Both attempts fail | Emergency compaction with a warning giving the tokens dropped |
| Conversation too large for one fork | Summarised in pieces, oldest prefix first; nothing dropped |
| One exchange larger than a fork | Clipped for the summary; tokens cut reported in a warning |
| Response spent its output budget thinking | Warns with the tokens used, compacts, asks again once |
| Request still too large after compaction | Turn fails fast with an actionable error instead of reaching the server |
| Tool result too large for cap | Truncated before being appended; omission notice included |
| `ToolResult` would be orphaned by a cut | The cut moves to the next clean boundary |
| Model loops in its reasoning | Stopped by the repetition detector (see the [thinking-loop watchdog](../reference/configuration.md#thinking-loop-watchdog)) |

See [parallel-tool-slots.md](parallel-tool-slots.md) for how tool calls from
one model response are executed concurrently and appended to the thread.
