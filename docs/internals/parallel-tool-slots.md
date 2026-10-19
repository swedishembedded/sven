# Parallel Tool Execution

This document explains how sven turns the tool calls of one model response
into concurrently running tool tasks, and how the results find their way back
into the conversation.

Three components share the work:

| Stage | Component | Responsibility |
|-------|-----------|----------------|
| Accumulation | `sven_turn::stream_turn` | Collect streamed tool-call chunks into complete `ToolCall`s. Executes nothing. |
| Dispatch | the machine's loop (`sven-machines`, `loop_core`) + the kernel | Emit one `Effect::CallTool` per proposed call and gate each through the permission policy. |
| Execution | `ToolExecutor` (`crates/executors/src/tool.rs`) | Run each allowed call on its own tokio task and report the result. |

---

## Accumulation - `stream_turn`

Providers stream tool calls as indexed chunks (`ResponseEvent::ToolCall
{ index, id, name, arguments }`). `stream_turn` keeps one accumulation slot per
index and appends each argument chunk to that slot's buffer.

Whenever a slot's buffer ends with `}`, the slot probe-parses it. The first
time the buffer parses as JSON, the call is complete: `AgentEvent::ToolCallStarted`
is emitted immediately, while the rest of the response is still streaming.

When the stream ends, every slot that never parsed is finalized:

1. Parse the buffer as-is.
2. Otherwise run `attempt_json_repair` (`crates/turn/src/tool_slots.rs`):
   fix invalid escape sequences, split fused keys, and close an open string
   and object.
3. If every repair fails, substitute `{}` and log a warning. The model's raw
   argument text is kept in the `ThreadStore` (`unparsed_arguments`), because the
   thread holds the call with `{}` arguments. The call is **not run**: the
   executor answers it with an error naming the parse failure (the first 120
   characters of what the model sent), so the model sees it and can repeat the
   call (`ToolRegistry::refuse_unparsed`, `take_refusal`).

Every call of a turn has an id of its own. A server that sends the same id for
all parallel calls (`call_0` each) gets `call_0`, `call_0_1`, `call_0_2`, ...:
results are paired with calls, and a call is marked unparsed, by id.

A slot whose buffer is empty also gets `{}`. A slot with an empty name is
dropped (it cannot be dispatched); a slot with an empty id gets a synthetic
`tc_synthetic_N` id, where `N` is the number of slots in the response.

### `<invoke>` fallback

Some models write tool calls as inline Anthropic-style XML instead of using the
structured function-call API. When a response produced no native tool calls
and its text contains `<invoke `, `stream_turn` extracts every
`<invoke name="...">...</invoke>` block as a tool call and removes it from the
text.

---

## Dispatch - `TurnExecutor`, the machine and the kernel

After the stream finishes, `TurnExecutor`:

1. Appends the assistant turn (text plus one tool-call message per call) to the
   conversation thread in the shared `ThreadStore`.
2. Records `call_id → (thread, original id)` so `ToolExecutor` can later append
   each result to the right thread under the exact id the model assigned.
3. Posts `Event::LlmTurnComplete { thread, text, tool_calls }` to the machine.

`TurnExecutor` also annotates every proposed call with the capability of its tool
(`ToolRegistry::capability_of_call`). The machine's loop
(`loop_core::on_llm_turn_complete`) emits one `Effect::CallTool` per proposed
call, records them in its pending set, and stays in its current state while they
run. The kernel classifies each `CallTool` individually against the session's
permission policy: allowed calls go straight to the executor, forbidden calls are
answered with `ToolFailed`, and calls that need a person's approval wait for it
(see [HSM Architecture](hsm-architecture.md#permission-policy---the-single-choke-point)).

---

## Execution - `ToolExecutor`

Before spawning, `ToolExecutor` fails the call at once (with `ToolFailed`) when
tools are disabled for the session (`--no-tools`), when its capability is not in
the executor's allow-list, when its registered tool exercises a different
capability than the call was authorized as (harness `tool` steps), or when the
model has not loaded the tool under deferred disclosure (the error tells it to
call the discovery tool with `action=load`).

Every other `CallTool` effect is spawned on its own tokio task
(spawn-and-forget), so the kernel's consumer loop returns immediately and all
calls from one response run concurrently. Each invocation is bounded by a
wall-clock watchdog (600 s) so a hung or panicking tool still produces a result
instead of leaving the call pending forever, and it is aborted if the run is
cancelled. The call's arguments are checked against the tool's parameter schema
and any violations are filed with the call's record in the trajectory; the call
still runs, and the tool answers a bad argument in its own words.

When a task finishes, `ToolExecutor`:

1. Emits `UiEvent::ToolCallFinished` with the full, untruncated output.
2. Appends the result to the call's thread under the id the model assigned,
   truncated by `sven_turn::smart_truncate` according to the tool's
   `OutputCategory` to `agent.tool_result_token_cap` tokens (default 4000, 0
   disables; see
   [prompt-compaction.md](prompt-compaction.md#layer-3---smart-tool-result-truncation)).
   An error result is stored with an `error: ` prefix.
3. Posts `Event::ToolSucceeded` or `Event::ToolFailed` to the machine. On
   success it first posts `ToolsActivated` and `InstructionsActivated` when the
   call loaded tools or touched files that activate instructions, so the next
   turn already offers them.

A tool that parks a question instead of answering emits no result: the executor
posts `Event::QuestionAsked`, and the call stays unresolved until a
`HumanAnswered` arrives.

Results arrive in completion order, not call order. The machine removes each
call from its pending set; once the set is empty it emits the next
`CallLlm`.

---

## Thread ordering

The assistant's tool-call messages are appended by `TurnExecutor` before any
tool task can finish, so every tool result in a thread follows the call it
answers. Before the next turn is sent, `TurnExecutor` also answers any call the
kernel refused before it ran (`TurnRequest::refused_calls`), so every tool call
in the thread has a result when the model sees it again.

---

## Event ordering

`AgentEvent` consumers (TUI, CI runner, ACP) see, per model pass that proposed
tool calls:

```
ToolCallStarted(call A)      <- as soon as A's arguments parse, mid-stream
ToolCallStarted(call B)
  ... tool progress events (ToolProgress, TodoUpdate, ModeChanged, ...) ...
ToolCallFinished(whichever call finishes first)
ToolCallFinished(the other)
```

`TurnComplete` is not emitted after such a pass: the machine stays in its state
and starts another pass once every call has a result. It is emitted only after a
pass that ends with a non-empty, tool-free answer, and `Aborted` is emitted
instead when the turn is cancelled.
