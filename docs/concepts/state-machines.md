# State Machine Reference

This document is the reference for the state machines in sven: their states,
the events each state handles, the effects each transition emits, the
permission policy each runs under, and how the `ModeRegistry` ties a mode name
to a machine. For the kernel that drives them see
[HSM Architecture](../internals/hsm-architecture.md); for what each user-facing
mode is for see [Agents and modes](agents.md).

---

## Overview

Sven ships six machines. Four are selected by a mode name through the
[`ModeRegistry`](#the-moderegistry); `TaskMachine` only ever runs as a child of
`SdlcMachine`, and `UiTestMachine` is registered only when its tool is compiled
in.

| Machine | Mode name(s) | Role |
|---------|--------------|------|
| [`ReactiveAgentMachine`](#reactiveagentmachine) | `agent`, `reactive`, `chat` | Streaming coding assistant; one turn per user message |
| [`SdlcMachine`](#sdlcmachine) | `sdlc` | Software-development lifecycle; drives many turns across phases |
| [`TaskMachine`](#taskmachine) | none (child of `SdlcMachine`) | One-shot worker for a single decomposed plan task |
| [`PredictMachine`](#predictmachine) | `predict` | One schema-constrained, tool-free turn per message (SDK) |
| [`VerifiedTaskMachine`](#verifiedtaskmachine) | `verified-task` | Attempts a task that an external verifier grades (`sven task run`) |
| [`UiTestMachine`](#uitestmachine) | `ui-test` (`android` feature) | Runs a fixed list of UI test steps on an Android device (`sven agent-dispatch`) |

Every machine obeys the same rules:

- A machine **never performs I/O**. It receives an `Event`, optionally mutates
  its `Context`, and returns a `Reaction` that contains zero or more `Effect`s.
- The kernel **validates** every `Effect` against the machine's
  `PermissionPolicy`, executes allowed effects on separate tasks, and posts
  result `Event`s back into the queue.
- The machines that let the model call tools (`ReactiveAgentMachine`,
  `SdlcMachine`, `TaskMachine`, `VerifiedTaskMachine`) share the
  [loop core](#the-shared-loop-core) for the in-state model-and-tool loop.
  There are no separate "running tools" or "awaiting approval" states: a state
  emits `CallTool` effects and stays where it is until the results arrive, so
  every call is gated against the state that really issued it.

---

## Event Vocabulary

Events are the **sole input** to a machine. Nothing reaches into the machine
directly; every external notification becomes a typed `Event` first
(`crates/hsm/src/event.rs`).

### Human events

| Event | Payload | Meaning |
|-------|---------|---------|
| `UserMessage` | `text` | A human sent a chat or instruction message |
| `UserProvidedArtifact` | `artifact` | A human attached an opaque artifact |
| `UserCancelled` | - | The human asked to cancel in-flight work. Also posted when a streaming turn is aborted (Esc, Ctrl+C, `/abort`) |
| `HumanApproved` | `approval_id` | A person approved a pending `RequestHumanApproval` |
| `HumanRejected` | `approval_id`, `reason?` | A person rejected it |
| `HumanAnswered` | `question_id`, `answer` | A parked question was answered (see `QuestionAsked`) |

### LLM events

| Event | Payload | Meaning |
|-------|---------|---------|
| `LlmTurnComplete` | `thread`, `text`, `tool_calls` | A streaming pass finished; the machine inspects `tool_calls` to decide its next action |
| `LlmFailed` | `error` | The model failed to produce a usable result |

`LlmProposedResponse`, `LlmProposedToolCall`, `LlmProposedPlan`,
`LlmProposedAssessment` and `DeliberationComplete` are part of the vocabulary
but no built-in machine or executor produces them.

### Tool events

| Event | Payload | Meaning |
|-------|---------|---------|
| `ToolSucceeded` | `call_id`, `observation` | A dispatched `CallTool` completed |
| `ToolFailed` | `call_id`, `error` | A `CallTool` failed, timed out, or was refused by the permission gate |
| `ToolApprovalRequired` | `call_id`, `capability`, `description` | The gate requires a person's approval for this call |
| `QuestionAsked` | `call_id`, `call_ref`, `prompt`, `options` | A tool parked a question for a person; the call stops counting as pending |
| `ToolsActivated` | `names` | A tool call loaded tools the model is offered in full from now on; the kernel records them in the context and no machine handles it ([tool-disclosure.md](../internals/tool-disclosure.md)) |
| `InstructionsActivated` | `ids` | Skills, subagents or knowledge documents became active; recorded by the kernel like `ToolsActivated` ([instruction-disclosure.md](../internals/instruction-disclosure.md)) |

### Other events

| Event | Payload | Meaning |
|-------|---------|---------|
| `Timeout` | `timer_id` | A scheduled timer elapsed |
| `VerificationComplete` | `verdict` | An `Effect::Verify` finished: `Passed`, `Failed`, `NeedsHuman` or `Unknown` |
| `EffectFailed` | `kind`, `error` | An effect was refused or had no executor and no more specific failure event exists |
| `Internal(Entry)` / `Internal(Exit)` / `Internal(Init)` | - | Reserved lifecycle signals: state entered, state left, fire a composite state's initial transition |
| `Internal(SubmachineCompleted)` | `machine`, `result` | A child run ended; `result` carries its output |
| `Internal(Custom)` | `name`, `payload` | A domain-defined internal signal |

---

## Effect Vocabulary

Effects are **pure data** returned by the machine. No machine calls I/O
directly; it asks the kernel to do it by returning an effect
(`crates/hsm/src/effect.rs`).

| Effect | Payload | Meaning |
|--------|---------|---------|
| `CallLlm` | `request` | Ask for one model pass; the result arrives as `LlmTurnComplete` or `LlmFailed` |
| `CallTool` | `call_id`, `name`, `capability`, `args` | Invoke a tool; the result arrives as `ToolSucceeded` or `ToolFailed` |
| `AskUser` | `prompt` | Ask the human; the answer arrives as `UserMessage` |
| `RequestHumanApproval` | `approval_id`, `capability`, `description`, `call?` | Put a call (manual approval) or a machine's own decision to a person; the answer arrives as `HumanApproved` or `HumanRejected`, at once and approved under auto approval |
| `RequestHumanAnswer` | `question_id`, `call_id`, `prompt`, `options` | Park a tool's question; the answer arrives as `HumanAnswered`, possibly in a later session |
| `ScheduleTimeout` / `CancelTimeout` | `timer_id`, `duration` / `timer_id` | Post `Timeout { timer_id }` after `duration` / cancel it |
| `PersistAudit` | - | Flush the audit trail to durable storage |
| `EmitInternal` | `name`, `payload` | Re-enter a domain signal into the event queue as `Internal(Custom)` |
| `InstantiateSubmachine` | `machine`, `descriptor` | Start a child run ([Parallel Submachine Fan-out](../internals/parallel-submachines.md)) |
| `Verify` | `spec` | Evaluate a declarative verifier against the real world; the verdict arrives as `VerificationComplete` |

### `CallLlm` requests

Every `CallLlm` request carries `kind = "turn"` and is handled by
`TurnExecutor`, the only component that calls a model:

- It streams one model response and appends the assistant turn (text and tool
  calls) to the named conversation thread of the `ThreadStore`.
- It executes no tool. It posts `LlmTurnComplete { thread, text, tool_calls }`
  (or `LlmFailed`) back into the kernel queue.
- The machine then decides what to do from `tool_calls` and the text alone.

The request fields are described in the
[Deliberation Engine](deliberation.md#the-turnrequest).

---

## Tool Capability Buckets

Every `CallTool` effect carries a `ToolCapability`, and the kernel's
`PermissionPolicy` checks it against what the machine's current state allows.
`InstantiateSubmachine` is checked as `SpawnChild`. A tool declares its
capability (`Tool::kernel_capability`, required), and a tool whose actions
differ in effect classes each call by its arguments (`Tool::call_capability`).

| Capability | Read-only? | Used by (examples) |
|------------|------------|--------------------|
| `ReadFile` | Yes | `read_file`, `find_file`, `grep`, `read_lints`, `buf_read`, `buf_grep`, `buf_status`, `todo`, `ask_question`, `skill`, `task`, `context` open/read/grep/expand, `memory` get/list/search_knowledge/list_knowledge, `semantic_memory` recall/list/get, `system` switch_mode/switch_model, `list_tasks`, `list_team` |
| `WriteFile` | No | `write_file`, `edit_file`, `memory` set/delete, `semantic_memory` remember/forget, `system` remove_mcp_server, team task and roster tools (`create_task`, `claim_task`, `complete_task`, `assign_task`, `update_task`, `create_team`, ...) |
| `DeleteFile` | No | `cleanup_team` |
| `ExecuteShell` | No | `shell`, the `gdb` tools, `system` add_mcp_server, `spawn_teammate` |
| `NetworkAccess` | No | `web_fetch`, `web_search`, `context` query/reduce, `context_query`, `context_reduce`, MCP tools |
| `GitOperation` | No | `merge_teammate_branch` (there are no `git_*` tools; the model runs git through `shell`) |
| `ControlDevice` | No | `android` |
| `RunVerifier` | Yes | Granted by `VerifiedTaskMachine` in `Verifying`; no tool or effect currently requires it |
| `SpawnChild` | Yes | `InstantiateSubmachine` (the `task` tool's sub-agent is admitted as a read) |

### Approval

What a state allows runs without anyone being asked unless the session runs
under **manual approval**, which is selected with `--approval manual` on the
interactive TUI. The default is `--approval auto`.

| Approval mode | Tool calls | A machine's own decision approval (`SdlcMachine` `need_approval`) |
|---------------|------------|--------------------------------------------------------------------|
| `auto` (default) | Every call the mode and state allow runs; nobody is asked | Approved at once by `UserExecutor` |
| `manual` | Every call whose capability is not read-only is put to the user, one call at a time, and runs only after a `HumanApproved` for that very call | Put to the user |

Under manual approval a `shell` call is not asked about when its command matches
`tools.auto_approve_patterns` (default `cat *`, `ls *`, `find *`, `rg *`,
`grep *`). A `shell` command matching `tools.deny_patterns` (default `rm -rf /*`,
`dd if=*`) is refused in both modes. Approval only narrows what a mode allows.

---

## ReactiveAgentMachine

Used by modes `agent`, `reactive` and `chat`
(`crates/machines/src/machines/reactive_agent.rs`).

### State Hierarchy

```
Top (root, never active leaf)
└── Session (composite)
    ├── Idle       <- waiting for the user's next message
    └── Generating <- model turn in flight; tools run in-state
```

### Permission Policy

`ReadFile`, `WriteFile`, `NetworkAccess`, `GitOperation` and `ExecuteShell` are
allowed in every state. `DeleteFile`, `ControlDevice`, `RunVerifier` and
`SpawnChild` are not. The host narrows this policy for the read-only session
modes: a `plan` or `research` session runs the same machine with `WriteFile`
removed, so the kernel refuses a file write even if the model proposes one (see
[Agents and modes](agents.md)).

### State Transitions

#### `Session` (composite parent of `Idle` and `Generating`)

| Event | -> State | Effects emitted |
|-------|----------|-----------------|
| `Internal(Init)` | `Idle` | - |
| `UserCancelled` | `Idle` | - |
| _(any other)_ | `Top` (super) | - |

#### `Idle`

| Event | -> State | Effects emitted |
|-------|----------|-----------------|
| `UserMessage { text }` | `Generating` | `CallLlm` on thread `chat` with `text` as the instruction (every tool the session offers) |
| _(any other)_ | `Session` (super) | - |

The round budget of the loop is the `agent.max_tool_rounds` configuration value
(default 200), handed to the machine as a context fact. Without the fact the
built-in budget is 16.

#### `Generating`

Tool events are handled **in-state** (`Reaction::Handled`): no transition occurs
while tools run.

| Event | Condition | -> State | Effects emitted |
|-------|-----------|----------|-----------------|
| `LlmTurnComplete`, no tool calls | text non-empty, no unresolved clarification | `Idle` | - (stores `last_response`) |
| `LlmTurnComplete`, no tool calls | text non-empty, the model asked the user a question and listed alternatives after it | _(stay)_ | one `CallTool` of `ask_question` carrying those alternatives |
| `LlmTurnComplete`, no tool calls | text empty | _(stay)_ | nudge `CallLlm`; after 3 consecutive empty turns, a wrap-up `CallLlm` without tools |
| `LlmTurnComplete`, tool calls | round within budget | _(stay)_ | one `CallTool` per call |
| `LlmTurnComplete`, tool calls | round over budget | _(stay)_ | wrap-up `CallLlm` without tools (sets `loop.max_rounds_reached`) |
| `ToolSucceeded` / `ToolFailed` | calls still pending | _(stay)_ | - |
| `ToolSucceeded` / `ToolFailed` | last pending call | _(stay)_ | continuation `CallLlm` |
| `ToolApprovalRequired` | - | _(stay)_ | `RequestHumanApproval` for that call |
| `HumanApproved` / `HumanRejected` | for the held tool call | _(stay)_ | approved: `CallTool` for the held call; rejected: continuation `CallLlm` that tells the model the call did not run |
| `QuestionAsked` / `HumanAnswered` | - | _(stay)_ | `RequestHumanAnswer` / continuation `CallLlm` |
| `LlmFailed`, `EffectFailed` | - | `Idle` | - (stores `last_error`) |
| _(any other)_ | - | `Session` (super) | - |

The clarification check is deliberately narrow: it fires only when the model
itself asked a user-directed question and itself listed two to six alternatives
after asking, and only when the session has `ask_question` registered. Any other
final answer ends the turn.

### Round-Trip Trace (single tool call)

```
UserMessage("fix the bug")
  -> [Idle] -> Generating + CallLlm(thread="chat", instruction="fix the bug")
  <- LlmTurnComplete(text="", tool_calls=[{read_file, ...}])
  -> [Generating] + CallTool(call_id=A, name="read_file", cap=ReadFile)
  <- ToolSucceeded(call_id=A, observation="...file contents...")
  -> [Generating] + CallLlm(thread="chat")
  <- LlmTurnComplete(text="The bug is on line 42...", tool_calls=[])
  -> [Generating] -> Idle
```

---

## SdlcMachine

Used by mode `sdlc` (`crates/machines/src/machines/sdlc/`). Every phase is a
*deliberation*: one instruction on the phase's own conversation thread, a
phase-scoped tool subset, and a structured decision that picks the transition.
The [Deliberation Engine](deliberation.md) page explains the model-facing side;
this section is the transition reference.

### State Hierarchy

```
Top (root, never active leaf)
├── Idle          <- waits for the first UserMessage
├── Intake        <- classify the request; clarify or confirm scope
├── Discovery     <- read-only exploration of the project
├── Planning      <- produce a plan and get it approved
├── Execution     <- implement the plan (write/shell tools); may fan out
├── Verification  <- independent build and test run
├── Delivery      <- summarise and get sign-off
├── Recovery      <- diagnose a failure; retry or give up
├── Done          <- terminal: work delivered
├── Failed        <- terminal: unrecoverable
└── Cancelled     <- terminal: user cancelled
```

All states are flat children of `Top`. `Top` handles `UserCancelled` for every
non-terminal state by moving to `Cancelled`. Terminal states ignore all events.

### Permission Policy (per state)

| Capability | `Idle` | `Intake` | `Discovery` | `Planning` | `Execution` | `Verification` | `Delivery` | `Recovery` |
|------------|--------|----------|-------------|------------|-------------|----------------|------------|------------|
| `ReadFile` | yes | yes | yes | yes | yes | yes | yes | yes |
| `GitOperation` | - | - | yes | yes | yes | - | yes | - |
| `WriteFile` | - | - | - | - | yes | - | - | - |
| `ExecuteShell` | - | - | - | - | yes | yes | - | - |
| `SpawnChild` | - | - | - | - | yes | - | - | - |
| `NetworkAccess`, `DeleteFile`, `ControlDevice` | - | - | - | - | - | - | - | - |

A call outside the current state's set is answered with `ToolFailed`
("permission denied") and never reaches the tool.

### Threads, Tools and Round Budgets (per phase)

| Phase | Thread | Tool subset | Max tool rounds |
|-------|--------|-------------|-----------------|
| `Intake` | `intake` | `read_file`, `find_file`, `grep` | 6 |
| `Discovery` | `discovery` | `read_file`, `find_file`, `grep` | 20 |
| `Planning` | `planning` | `read_file`, `find_file`, `grep` | 16 |
| `Execution` | `execution` | `read_file`, `find_file`, `grep`, `write_file`, `edit_file`, `shell` | 40 |
| `Verification` | `verification` | `read_file`, `find_file`, `grep`, `shell` | 20 |
| `Delivery` | `delivery` | `read_file`, `find_file`, `grep` | 12 |
| `Recovery` | `recovery` | `read_file`, `find_file`, `grep` | 12 |
| `TaskMachine` child | `task` | same as `Execution` | 40, lowered to `agent.max_tool_rounds` when that is smaller |

A model that exhausts a phase's round budget is told to stop calling tools and
answer; that answer is then parsed as the phase's decision like any other.

### Decision Schema

Every phase asks the model to answer with a JSON object (all fields except
`status` are optional):

```json
{
  "status": "proceed | need_user_input | need_approval | need_tools | failed",
  "summary": "one-sentence summary",
  "message": "user-facing text",
  "questions": ["clarifying questions"],
  "approval_prompt": "what the developer is asked to approve",
  "payload": { }
}
```

The machine maps `status` to a transition:

| `status` | Transition |
|----------|------------|
| `proceed` | store the phase result; advance to the next phase |
| `need_user_input` | emit `AskUser` (built from `message` and `questions`); stay in the phase |
| `need_approval` | store the phase result; emit `RequestHumanApproval` (capability `GitOperation`); stay until the answer |
| `need_tools` | emit another continuation `CallLlm`; stay in the phase |
| `failed` | go to `Recovery` |

Parsing is fail-safe. A missing or unknown `status` counts as `failed`, text that
is not parseable as a JSON object sends the phase to `Recovery`, and a model that
wraps the JSON in code fences or prose is tolerated.

### State Transitions

#### `Idle`

| Event | -> State | Effects emitted |
|-------|----------|-----------------|
| `UserMessage { text }` | `Intake` | - (stores `user_request`; `Intake` issues its `CallLlm` on entry) |
| _(any other)_ | `Top` (super) | - |

#### Standard phases: `Intake`, `Discovery`, `Planning`, `Verification`, `Delivery`

| Event | Condition | -> State | Effects emitted |
|-------|-----------|----------|-----------------|
| `Internal(Entry)` | - | _(stay)_ | `CallLlm` with the phase's instruction |
| `LlmTurnComplete`, tool calls | within budget | _(stay)_ | one `CallTool` per call |
| `LlmTurnComplete`, no tool calls | `status` per the schema table | see above | see above |
| `ToolSucceeded` / `ToolFailed` | last pending call | _(stay)_ | continuation `CallLlm` with the decision schema |
| `ToolApprovalRequired` | - | _(stay)_ | `RequestHumanApproval` for the call |
| `HumanApproved` | for a held tool call | _(stay)_ | `CallTool` for the held call |
| `HumanApproved` | for the phase decision | next phase | - |
| `HumanRejected` | for a held tool call | _(stay)_ | continuation `CallLlm` |
| `HumanRejected` | for the phase decision | see table below | see table below |
| `UserMessage { text }` | - | _(stay)_ | follow-up `CallLlm` carrying the answer |
| `LlmFailed`, `EffectFailed` | - | `Recovery` | - |

| Phase | `proceed` / approved target | Rejected decision approval |
|-------|-----------------------------|----------------------------|
| `Intake` | `Discovery` | stay; revision turn |
| `Discovery` | `Planning` | `Recovery` |
| `Planning` | `Execution` | stay; revision turn |
| `Verification` | `Delivery` | `Recovery` |
| `Delivery` | `Done` | stay; revision turn |

The phase prompts steer the model so the approval gates fall where a person
should decide: `Intake` asks for confirmation of the scope, `Planning` asks for
approval of the plan, and `Delivery` asks for acceptance of the delivered work.
`Discovery` and `Verification` normally answer `proceed` (or `failed`).
Under `--approval auto` (the default) the kernel approves these gates at once;
under `--approval manual` they go to the user. Headless runs are always auto.

#### `Execution`

| Event | Condition | -> State | Effects emitted |
|-------|-----------|----------|-----------------|
| `Internal(Entry)` | plan has 2 or more tasks and a child spawner is installed | _(stay)_ | one `InstantiateSubmachine` per task |
| `Internal(Entry)` | otherwise | _(stay)_ | `CallLlm` with the execution instruction |
| `Internal(SubmachineCompleted)` | children still running | _(stay)_ | - |
| `Internal(SubmachineCompleted)` | last child done | _(stay)_ | follow-up `CallLlm` listing every child's summary, asking the model to integrate and confirm |
| `LlmTurnComplete`, no tool calls | `proceed` | `Verification` | - |
| `HumanApproved` | - | _(stay)_ | follow-up `CallLlm` ("Approved. Continue implementing.") |
| `HumanRejected` | decision approval | `Recovery` | - |
| `LlmFailed`, `EffectFailed` | - | `Recovery` | - |

Other events (tool results, `UserMessage`, the remaining `status` values) behave
as in the standard phases. A task list entry in the plan's `payload.tasks` is a
string or an object with a `title`, `task` or `description` field (the planning
prompt asks for `{"id", "description"}` objects). Fan-out is described in
[Parallel Submachine Fan-out](../internals/parallel-submachines.md).

#### `Recovery`

`Recovery` diagnoses the failure that sent the machine there. `to_recovery`
records the failing phase as `failed_phase` and the reason as `failure_context`.

| Event | Condition | -> State | Effects emitted |
|-------|-----------|----------|-----------------|
| `Internal(Entry)` | recovery counter 1 to 3 | _(stay)_ | `CallLlm` with the failure context |
| `Internal(Entry)` | recovery counter above 3 | `Failed` | - |
| `LlmTurnComplete`, no tool calls | `proceed` | the failed phase | - (the phase starts over from its entry) |
| `LlmTurnComplete`, no tool calls | `need_user_input` | _(stay)_ | `AskUser` |
| `LlmTurnComplete`, no tool calls | any other status, or unparseable | `Failed` | - |
| `LlmTurnComplete`, tool calls | within budget | _(stay)_ | one `CallTool` per call |
| `LlmTurnComplete`, tool calls | over budget | `Failed` | - |
| `UserMessage { text }` | - | _(stay)_ | follow-up `CallLlm` |
| `LlmFailed`, `EffectFailed` | - | `Failed` | - |

The recovery counter is the context's `retry_counters["recovery"]`. It counts
entries over the whole session and is never reset, so one session recovers at
most three times in total (`MAX_RECOVERY`).

#### Terminal states

| State | Reached by |
|-------|------------|
| `Done` | `Delivery` accepted |
| `Failed` | `Recovery` gave up or exhausted its counter |
| `Cancelled` | `UserCancelled` in any non-terminal state |

### Round-Trip Trace

```
UserMessage("add JSON logging")
  -> [Idle] -> Intake                       (user_request stored)
  -> [Intake/entry] + CallLlm(thread="intake")
  <- LlmTurnComplete(tool_calls=[{read_file, "Cargo.toml"}])
  -> [Intake] + CallTool(A, read_file)
  <- ToolSucceeded(A)
  -> [Intake] + CallLlm(thread="intake")
  <- LlmTurnComplete(text='{"status":"need_approval","approval_prompt":"Add JSON logging to X?"}')
  -> [Intake] + RequestHumanApproval(GitOperation)
  <- HumanApproved
  -> [Intake] -> Discovery + CallLlm(thread="discovery")
  <- LlmTurnComplete(text='{"status":"proceed",...}')
  -> [Discovery] -> Planning + CallLlm(thread="planning")
  <- LlmTurnComplete(text='{"status":"need_approval","payload":{"tasks":[...]}}')
  -> [Planning] + RequestHumanApproval
  <- HumanApproved
  -> [Planning] -> Execution               (write/shell tool rounds in-state)
  <- LlmTurnComplete(text='{"status":"proceed",...}')
  -> [Execution] -> Verification -> Delivery
  <- LlmTurnComplete(text='{"status":"need_approval",...}')
  <- HumanApproved
  -> [Delivery] -> Done
```

---

## TaskMachine

A one-shot child run started by `SdlcMachine` in `Execution` to implement a single
decomposed task (`crates/machines/src/machines/sdlc/task.rs`). It runs in its own
kernel with a fresh `Context`, so its thread and facts are isolated from the parent.

### State Hierarchy

```
Top (root)
├── Run  <- the task's turn loop, tool calls and any wait for a person
└── Done <- terminal; the result is stored in the context fact "out"
```

### Permission Policy

None of its own. The child runs under the contract its parent's state hands down
(what the parent's policy allows in `Execution`), narrowed by the spawner to
`ReadFile`, `WriteFile`, `ExecuteShell`, `GitOperation` and `NetworkAccess`. A
task never holds `SpawnChild`. A call outside the contract resolves as
`ToolFailed`. See [Parallel Submachine Fan-out](../internals/parallel-submachines.md).

### State Transitions

| Event | Condition | -> State | Effects emitted |
|-------|-----------|----------|-----------------|
| `Internal(Entry)` | - | _(stay)_ | `CallLlm` on thread `task` with the task text |
| `LlmTurnComplete`, no tool calls | `proceed`, `need_tools` or `failed` | `Done` | - (`ok` is true only for `proceed`) |
| `LlmTurnComplete`, no tool calls | `need_user_input` / `need_approval`, parent's gates reachable | _(stay)_ | `AskUser` / `RequestHumanApproval` |
| `LlmTurnComplete`, no tool calls | `need_user_input` / `need_approval`, no gates | `Done` | - (`ok: false`, summary says a person was needed) |
| `LlmTurnComplete`, no tool calls | unparseable | `Done` | - (`ok: false`) |
| `LlmTurnComplete`, tool calls | within budget | _(stay)_ | one `CallTool` per call |
| `LlmTurnComplete`, tool calls | over budget | `Done` | - (`ok: false`, "max tool rounds reached") |
| `ToolSucceeded` / `ToolFailed` / `ToolApprovalRequired` | - | _(stay)_ | as in the loop core |
| `UserMessage` | task was waiting for an answer | _(stay)_ | follow-up `CallLlm` |
| `HumanApproved` | - | _(stay)_ | follow-up `CallLlm` |
| `HumanRejected` | - | `Done` | - (`ok: false`) |
| `LlmFailed`, `EffectFailed` | - | `Done` | - (`ok: false`) |

### Result format (`out`)

```json
{
  "task":    "implement the read_file tool",
  "summary": "Added read_file.rs and registered it in the registry",
  "ok":      true,
  "payload": { }
}
```

The parent harvests it from `SubmachineCompleted { result }`.

---

## PredictMachine

Mode `predict` (`crates/machines/src/machines/predict.rs`) runs one
schema-constrained turn with no tools. It is the machine behind the SDK's
`Strategy::Predict` ([SDK reference](../reference/sdk.md)). The caller seeds the
context facts `predict.schema` and `predict.schema_name` before the first message.

```
Top
└── Session            (UserCancelled -> Idle)
    ├── Idle           <- waiting for a task
    └── Generating     <- one constrained turn in flight
```

| Event | State | -> State | Effects / result |
|-------|-------|----------|------------------|
| `UserMessage { text }` | `Idle` | `Generating` | `CallLlm` on thread `predict`, schema attached, no tools |
| `LlmTurnComplete { text }` | `Generating` | `Idle` | stores `text` in fact `predict.candidate` |
| `LlmFailed`, `EffectFailed` | `Generating` | `Idle` | stores the error in fact `predict.error` |

The machine never validates the candidate. Validation and any repair attempt are
the caller's: a repair arrives as the next `UserMessage`. Its registered policy
allows no capability.

---

## VerifiedTaskMachine

Mode `verified-task` (`crates/machines/src/machines/verified_task.rs`) is started
by `sven task run <file>.task.toml`. The model's final turn only means "I believe
I am finished"; whether that is true is decided by an external verifier
(`Effect::Verify` answered by `VerificationComplete`), never by parsing the
model's text.

```
Top
├── Freeze      <- parses the first UserMessage (the task seed) and pins the verifier
├── Attempting  <- the ordinary tool loop (thread "verified_task", 40 rounds per attempt)
├── Verifying   <- waits for VerificationComplete
├── Retrying    <- bounces back into Attempting for the next attempt
├── Done        <- terminal: passed, retries exhausted, or a setup error
└── Parked      <- terminal: the verifier could not decide (NeedsHuman / Unknown)
```

| Event | State | -> State | Effects |
|-------|-------|----------|---------|
| `UserMessage` (seed JSON) | `Freeze` | `Attempting` | - (malformed seed: `Done` with fact `verified_task_error`) |
| `LlmTurnComplete`, no tool calls | `Attempting` | `Verifying` | `Verify { spec }` with the frozen verifier |
| `LlmTurnComplete`, over budget | `Attempting` | `Retrying`, or `Done` when attempts are exhausted | - |
| `LlmFailed`, `EffectFailed` | `Attempting` | `Done` | - (sets `verified_task_error`) |
| `VerificationComplete(Passed)` | `Verifying` | `Done` | sets `verified_task_verdict` `{"passed": true}` |
| `VerificationComplete(Failed)` | `Verifying` | `Retrying`, or `Done` (`{"passed": false}`) when attempts are exhausted | - |
| `VerificationComplete(NeedsHuman / Unknown)` | `Verifying` | `Parked` | sets `verified_task_needs_human` |
| `EffectFailed` | `Verifying` | `Done` | the verifier could not run; no verdict is recorded |

Policy: `ReadFile` everywhere; `WriteFile`, `ExecuteShell` and `GitOperation` in
`Attempting`; `RunVerifier` in `Verifying`. The retry budget is the task's
`max_attempts`.

---

## UiTestMachine

Mode `ui-test` drives a fixed list of natural-language UI test steps on an Android
device through the `android` tool (`crates/machines/src/machines/ui_test/`). It is
registered only in builds with the `android` feature, and is started by
`sven agent-dispatch`. The step order is fixed by the test author; the model only
compiles each step into one verb (`launch_app`, `force_stop`, `tap`, `type_text`,
`swipe`, `key_event`, `wait`, `ask_user`).

```
Top
├── Seeding    <- parses the first UserMessage as the step list
├── Compiling  <- one schema-constrained CallLlm compiles the current step
├── Locating   <- resolves the target, or records a baseline view-hierarchy digest
├── Acting     <- the android tool call (or ask_question) for the step
├── Verifying  <- re-reads the hierarchy; an action that changed nothing spends a retry
├── Retrying   <- bounces back into Compiling for the same step
├── Done       <- terminal: every step passed
└── Failed     <- terminal: malformed script, or a step used up its 3 attempts
```

Its policy allows `ReadFile` and `ControlDevice` and asks no approval, so
`--approval manual` is refused for it.

---

## The Shared Loop Core

`ReactiveAgentMachine`, `SdlcMachine`, `TaskMachine` and `VerifiedTaskMachine`
compose `machines::loop_core` instead of duplicating the model-and-tool loop
(`crates/machines/src/machines/loop_core.rs`).

| Helper | Purpose |
|--------|---------|
| `init_loop(ctx, thread, tools, all_tools_mode, max_rounds)` | Initialise the `LoopState` when a state starts a loop |
| `build_turn_effect(...)` | Build the canonical `CallLlm { kind: "turn" }` effect |
| `on_llm_turn_complete(ctx, event)` | Count the round and classify a `LlmTurnComplete` as a `GeneratingAction` |
| `handle_tool_event(ctx, make_turn, event)` | Handle `ToolSucceeded`, `ToolFailed`, `ToolApprovalRequired`, `QuestionAsked`, `HumanAnswered` and tool-level `HumanApproved` / `HumanRejected` in-state; returns `None` for any other event, including approvals of a machine's own decision |

### `GeneratingAction`

| Variant | Condition | Machine action |
|---------|-----------|----------------|
| `FinalAnswer` | no tool calls, non-empty text | end the turn: `Idle` (reactive), parse the decision (SDLC, task) |
| `CallTools` | tool calls, round within budget | emit the `CallTool` effects; stay |
| `EmptyTurn` | no tool calls, empty text | emit a nudge `CallLlm`; stay |
| `MaxRoundsReached` | tool calls over budget, or more than 3 consecutive empty turns | emit a wrap-up `CallLlm` without tools; stay |

### `LoopState`

One typed value stored under the context fact `lc_state`:

| Field | Description |
|-------|-------------|
| `thread`, `tools`, `all_tools_mode` | Conversation thread, allowed tool names, and the mode used to resolve tools when `tools` is empty |
| `round`, `max_rounds` | Rounds used and the budget |
| `pending` | Call ids of in-flight `CallTool` effects |
| `awaiting_tool_approval` | Set while a tool-level approval is pending; it distinguishes a tool approval from a machine's own decision approval |
| `awaiting_answer` | Set while a tool call is parked on a question |
| `proposed`, `call_registry` | The current batch of calls as proposed (so an approved call runs exactly as proposed) and their fingerprints |
| `failure_streaks`, `stall_nudge` | Consecutive identical failures per call; after 2, the next continuation turn tells the model to change approach |
| `refusals` | Why calls failed or were refused, handed to the next turn so every call has an answer |
| `empty_turns` | Consecutive empty turns |

Continuation turns also tell the model where it stands in its round budget: once
at half the budget and once when about a fifth remains.

---

## The ModeRegistry

`ModeRegistry` (`crates/machines/src/mode.rs`) maps a mode string to a
`MachineRegistration`: a factory that builds a fresh type-erased machine, and
the `PermissionPolicy` every session running that mode enforces. The policy
travels with the registration; a mode the registry does not hold has no policy
and cannot run.

`ModeRegistry::default_registry()` registers machines that need no particular
tool:

| Mode string | Machine | Registered policy |
|-------------|---------|-------------------|
| `agent`, `reactive`, `chat` | `ReactiveAgentMachine` | read, write, network, git, shell in every state |
| `sdlc` | `SdlcMachine` | per-state, see [above](#permission-policy-per-state) |
| `verified-task` | `VerifiedTaskMachine` | as described above |
| `predict` | `PredictMachine` | nothing |

A machine that drives one specific tool is registered by the assembly that
compiles the tool in. `sven-bootstrap` (`crates/bootstrap/src/modes.rs`) adds
`ui-test` when the `android` feature is on; a build without it neither lists nor
runs that mode.

The session assembler then applies what only it knows: it narrows the policy for
the read-only session modes (`plan`, `research`) and puts it under the session's
approval mode.

### How a mode name is chosen

- Interactive and headless sessions: `--mode` takes `research`, `plan`, `agent`
  (default), `chat` or `sdlc`. `sdlc` runs `SdlcMachine`, `chat` runs the `chat`
  mode, and `agent`, `plan` and `research` all run the `agent` machine. `plan` and
  `research` differ only by the host removing `WriteFile`.
- A headless run reads `SVEN_MODE` as the kernel mode name and so can select any
  registered mode, such as `verified-task`.
- `sven task run` selects `verified-task`, `sven agent-dispatch` selects `ui-test`,
  and the SDK selects `predict` or `agent` from the strategy of a typed method.

---

## How the Machines Relate

```
CLI / TUI / SDK
   |  UserMessage
   v
Kernel runtime ------------------------------------------------+
   |                                                           |
   |  dispatches the event to the machine the mode selected    |
   v                                                           |
ReactiveAgentMachine          SdlcMachine                      |
  Top -> Session                Top (flat)                     |
    Idle                          Idle                         |
    Generating                    Intake                       |
    (tool loop in-state)          Discovery                    |
                                  Planning                     |
                                  Execution --InstantiateSubmachine--> TaskMachine
                                  Verification                 |        (child kernel)
                                  Delivery                     |          Run -> Done
                                  Recovery                     |
                                  Done / Failed / Cancelled    |
   |
   |  returns Vec<Effect>
   v
PermissionPolicy (CallTool is classified per call)
   |
   +-- Allowed        -> ToolExecutor -> ToolSucceeded / ToolFailed
   +-- NeedsApproval  -> ToolApprovalRequired -> in-state approval gate
   +-- Forbidden      -> ToolFailed (synthesised; never dispatched)
```

**`TurnExecutor`** is the only component that calls the model. It streams a
single response, accumulates tool-call JSON slot by slot without executing
anything, appends the assistant turn to the `ThreadStore` thread, and posts
`LlmTurnComplete`.

**`ToolExecutor`** is the only component that runs tools. It runs each
`CallTool` on its own task, appends results to the thread, and posts
`ToolSucceeded` or `ToolFailed`. See
[Parallel Tool Execution](../internals/parallel-tool-slots.md).
