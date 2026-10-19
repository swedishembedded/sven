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
`crates/executors/src/turn.rs`); the pure building blocks live in
`sven-turn` (`crates/turn/src/compact.rs`) and `sven_model::budget`.

```
CallLlm { kind: "turn" } effect
  │
  ▼
TurnExecutor: snapshot the thread, resolve tool schemas
  │
  ▼
estimate_request_tokens(messages, tool_schemas, dynamic_suffix)
  │
  ▼
estimate / input_budget ≥ trigger_threshold?     ← only when the window is known
  ├─ no  → continue
  └─ yes → compact_thread()
             ├─ nothing older than the kept tail → unchanged
             ├─ summary request fits → tool-free summarization turn
             │     └─ call fails / empty text → emergency_compact()
             └─ summary request does not fit → emergency_compact()
  │
  ▼
estimate > input_budget?  → fail the turn with an actionable error (hard gate)
  │
  ▼
stream_turn()                 ← model call
  │
  ▼
ToolExecutor (per tool call, concurrently)
  └─ smart_truncate()         ← cap each result before it enters the thread
```

---

## Layer 1 - Token accounting (`sven_model::budget`)

### The input budget

`effective_input_budget(context_window, max_output_tokens)` returns the usable
input ceiling: the model's context window minus a minimal output reserve
(256 tokens). It does **not** subtract the full configured output cap - the
output limit actually requested for a call is computed separately by
`dynamic_output_budget`, scaled to whatever room the real prompt leaves.

When the context window is not known at all (a hosted provider with no catalog
entry and no live probe result), the budget is `None` and both the compaction
trigger and the hard gate are skipped rather than guessing.

### The request estimate

`estimate_request_tokens` covers the whole request, not just the history:

```
raw      = Σ approx_tokens(message)                      // chars / 4
         + Σ (len(name + description + parameters)) / 4  // tool schemas
         + len(dynamic_suffix) / 4
estimate = raw × 1.1                                     // headroom
```

The 10% headroom errs toward compacting (or rejecting) slightly early, which
is far cheaper than a request the server rejects after admission.

---

## Layer 2 - Proactive compaction

Before every model turn, `TurnExecutor` compares the estimate with the budget:

```
trigger_threshold = max(compaction_threshold − compaction_overhead_reserve, 0)
compact when       estimate / input_budget ≥ trigger_threshold
```

The overhead reserve (default 10%) keeps enough room for the summarization
request itself.

### Normal path

1. `prepare_compaction` splits the non-system messages into the older part to
   summarize and the most recent `compaction_keep_recent` messages to keep
   verbatim, moving the split so the kept tail never starts inside a
   tool-call/tool-result group (see [Split safety](#split-safety-for-tool-callresult-pairs)).
   If nothing is older than the kept tail, compaction cannot help and the turn
   proceeds unchanged.
2. The older part is serialized into a single summarization request using the
   configured strategy's prompt.
3. If that request fits the budget, a **tool-free** `stream_turn` call
   produces the summary. Its text deltas go to a throwaway channel so they
   never reach the observation plane as if the agent had said them.
4. `finish_compaction` rebuilds the history as
   `[system, assistant(summary), ...kept tail]`.
5. The thread is replaced in the `ThreadStore` and the in-flight turn
   continues with the smaller history immediately.

### Emergency path

`emergency_compact` runs instead when the summarization request would not fit
the budget, or when the summarization call fails or returns empty text:

- Drop all non-system messages except the last `compaction_keep_recent`
  (again adjusted to a clean tool-group boundary).
- Prepend a canned assistant notice telling the model that earlier history was
  dropped.
- No model call - always succeeds regardless of session size.
- Reported as `CompactionStrategyUsed::Emergency`.

A compaction failure is never propagated to the caller; the turn continues
with whatever history compaction produced.

### The hard gate

After compaction, if the estimate still exceeds the input budget, the turn
fails immediately with an error naming the estimate, the context window, the
provider and the model, instead of sending a request the server would reject.

---

## Compaction strategies

The strategy is selected by the `compaction_strategy` config key.

### Structured (default)

The compaction prompt instructs the model to produce exactly six Markdown
sections. The model is not allowed to add or remove sections:

```markdown
## Active Task
## Key Decisions & Rationale
## Files & Artifacts
## Constraints & Requirements
## Pending Items
## Session Narrative
```

Technical details - file paths, function names, error messages, code snippets,
test names - are preserved verbatim within those sections. The result is a
checkpoint that the model can reference reliably on subsequent turns.

### Narrative

A free-form prose summary of the conversation. Useful for highly
conversational sessions where structured sections add little value.

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
message. If compaction summarised the `tool_use` messages but preserved their
`tool_result` messages in the kept tail, the next API call would fail with:

```
messages.2.content.0: unexpected `tool_use_id` found in `tool_result` blocks
```

`clean_boundary_start` therefore starts the kept tail at `len − keep_recent`
and walks it forward past any leading `ToolCall`/`ToolResult` messages:

```rust
while start < messages.len() {
    match &messages[start].content {
        ToolResult { .. } | ToolCall { .. } => start += 1,
        _ => break,
    }
}
```

The kept tail therefore always begins on a user message or an
assistant-text message - never mid-batch. Both the normal and the emergency
path use this boundary.

---

## Event reporting

Every compaction emits `UiEvent::ContextCompacted` (`AgentEvent` is the same
type):

```rust
ContextCompacted {
    tokens_before: usize,             // history estimate before compaction
    tokens_after:  usize,             // history estimate after rebuild
    strategy: CompactionStrategyUsed, // Structured | Narrative | Emergency
    turn: u32,                        // 0 for compaction before a model turn
}
```

The TUI displays this in the chat pane. CI logs it to stderr as:

```
[sven:context:compacted:structured] 60674 → 7144 tokens
```

---

## Configuration reference

All fields live under the `agent` section of the sven configuration
(`.sven.yaml`, `.sven/config.yaml`, or `~/.config/sven/config.yaml`).

| Key | Default | Description |
|---|---|---|
| `compaction_threshold` | `0.85` | Fraction of the input budget that triggers compaction |
| `compaction_keep_recent` | `6` | Number of non-system messages preserved verbatim (≈ 3 back-and-forth turns) |
| `compaction_strategy` | `structured` | `structured` or `narrative` |
| `tool_result_token_cap` | `4000` | Per-result token ceiling before truncation (`0` disables) |
| `compaction_overhead_reserve` | `0.10` | Safety margin subtracted from `compaction_threshold` |

### Tuning for CI / long-running workflows

```yaml
agent:
  compaction_threshold: 0.65       # fire earlier, more margin for schema overhead
  compaction_keep_recent: 15       # keep more recent context for multi-step plans
  compaction_strategy: structured  # structured checkpoint survives many turns
  tool_result_token_cap: 2000      # tighter cap if build logs are large
  compaction_overhead_reserve: 0.12
```

### Tuning for interactive sessions

```yaml
agent:
  compaction_threshold: 0.80       # allow longer free-form conversation
  compaction_keep_recent: 5
  compaction_strategy: narrative   # prose summary reads more naturally in TUI
  tool_result_token_cap: 8000      # more context per file read
```

---

## Compaction failure modes and mitigations

| Failure | What sven does |
|---|---|
| Summarization call fails (network, rate limit) | Falls back to emergency compaction; does not propagate the error |
| Summarization returns empty text | Same fallback as above |
| Summarization request itself would not fit | Emergency compaction directly, no model call |
| Request still too large after compaction | Turn fails fast with an actionable error instead of reaching the server |
| Tool result too large for cap | Truncated before being appended; omission notice included |
| `ToolResult` would be orphaned by the split | Kept tail starts at the next clean boundary |

See [parallel-tool-slots.md](parallel-tool-slots.md) for how tool calls from
one model response are executed concurrently and appended to the thread.
