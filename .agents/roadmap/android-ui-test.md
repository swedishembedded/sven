# android-ui-test

**Status: phase 1 done (device control), phase 2 done (brain grounding),
phase 3 done against mocks (sven machine + tools) and now wired to a real
CLI entry point (`sven agent-dispatch`, see this file's own update below) -
still no real-device/real-checkpoint pass through that entry point (the new
integration test self-skips without hardware, matching
`live_ground.rs`/`live_device.rs`'s convention), phase 4 done for the
local/single-worker dispatch path (whale) including real `Link`-input
data-flow into Agent-node dispatch and real-graph orchestrator-tier device
placement wiring (both closed in a later whale-only session, see Phase 4's
own update below) - whale's own dispatch path is still only proven against
the generic smoke-test dispatcher, not yet re-pointed at
`sven agent-dispatch` end to end (a whale-side config change, not a sven-side
gap).**

## Goal

A reproducible UI-test workflow: drive a real Android app through a
declared sequence of natural-language steps (`Launch the demo app`, `Click
"log in with password"`, `Enter the code`, ...), using a vision model as a
bounded grounding oracle ("where is element X on this screenshot") rather
than an open-ended reasoning agent - so a run is deterministic enough to
gate CI on, not just an autonomous phone agent. Whale schedules it on a
node with an attached device and reports step count/timing/failure point.

## Why sven, not just whale

The step sequence is fixed by the test author; the HSM's job is executing
it deterministically (screenshot -> ground -> act -> verify -> next),
retrying/failing on a bounded budget. That's a `Machine` implementing its
own state list directly - see `crates/machines/src/machines/reactive_agent.rs`
for the shape to imitate; `loop_core` is NOT reusable here, it's built
entirely around "ask the LLM what to do next" (`Event::LlmTurnComplete`).

## Phase 1 - done

`sven-tools-android` (`crates/tools-android`): typed ADB verb set
(screenshot/tap/swipe/type_text/key_event/go_home/launch_app/force_stop/
list_packages/display_info/current_app/wait), `ToolCapability::ControlDevice`
as its own permission bucket (never `ExecuteShell`). Verified against a
real device, including a manual drive through the target app's real login +
add-card flow (see commit history).

**Confirmed constraint, not yet handled anywhere:** any view an app marks
`FLAG_SECURE` returns a solid-black `screencap` - a payment confirmation, an
authenticator app or DRM video, not only a login screen - and a grounding
model can never see it. Any step touching such a screen is a hard
human-in-the-loop boundary, not a modeling gap to solve later. The
`ask_question` tool (`sven-tools-agent`) is the existing mechanism this
should hand off to; the step compiler (phase 3) needs a named-variable
result ("code") a later step can reference.

## Phase 2 - done (brain)

`brain/florence2`'s `ground` capability action is implemented and tested in
the brain repo: `crates/florence2/src/caps.rs` (`ground_spec`/
`FlorenceSession::ground`/`Florence2Provider`), reachable as
`brain florence2 ground --target "<phrase>" --in image=<path> [--json]` on
the CLI (via `crates/cli/src/resolve.rs`'s arch-id dispatch) or over D-Bus
as `Brain1.Manager.Run("brain/florence2", "ground", ...)`. Output is
`{found, boxes: [{phrase, bbox}]}`, `bbox` normalized `[x0,y0,x1,y1]` in
`[0,1]` - the shape sven's `ground` tool (Phase 3, below) parses directly.
Not re-verified against a live checkpoint by this Phase 3 pass (that
belongs to brain's own test suite); sven's integration is checked against
brain's real CLI contract, not just a guess at its shape.

## Phase 3 - done against mocks (sven)

`UiTestMachine` implements `Machine` directly (own state list: `Top` /
`Seeding` / `Compiling` / `Locating` / `Acting` / `Retrying` / `Done` /
`Failed` - not `loop_core`), in
`crates/machines/src/machines/ui_test/{mod,step,vars}.rs`, plus a new
domain-tier crate `sven-tools-ground` (`crates/tools-ground`) providing the
`ground` tool that wraps `brain florence2 ground`. All three required
pieces are implemented and covered by unit/machine-dispatch tests running
against fakes (a fake `brain` subprocess, synthetic screenshots, and
directly-dispatched `LlmTurnComplete`/`ToolSucceeded`/`ToolFailed` events -
no real device or brain checkpoint needed for the default test run):

1. **Step compiler** (`step.rs`): one bounded, schema-constrained `CallLlm`
   turn (`TurnRequest` with an empty tool set and a JSON response schema -
   the generic structured-output mechanism any machine can use, not
   `loop_core`'s stateful tool loop) turns one natural-language step line
   (free-form prose in whatever language the script is written in, e.g.
   "Launch the demo app", `Click "log in with password"`, "Enter the code")
   into `{verb, target, value, value_ref, bind}`.
   Verb set: `launch_app`/`force_stop`/`tap`/`type_text`/`swipe`/
   `key_event`/`wait`/`ask_user`.
2. **Variable binding** (`vars.rs`): `ask_user` answers are nameable
   (case/whitespace-insensitive) and stored as a `Context` fact; a later
   step's `value_ref` resolves against it, so "Ask the user for the code"
   binding `code` and a later "Enter the code" compiling to
   `{verb: type_text, value_ref: "code"}` actually thread the value
   through end to end (tested).
3. **Secure-screen (`FLAG_SECURE`) hand-off**: `sven-tools-ground`'s `ground`
   tool detects a solid-black `FLAG_SECURE` screenshot locally (pixel-sampling
   heuristic, `black_screen.rs`) and returns `{secure_screen: true}`
   *without ever invoking brain* - the grounding model genuinely never sees
   it, not just "sven ignores the answer". `UiTestMachine`'s `Locating`
   state checks that flag and routes to `Acting`'s `ask_question` path
   instead of attempting a tap, using the same `ask_question` tool
   `reactive_agent.rs`'s clarification post-check already uses.

### What's verified vs. what's still open

Verified (mocked): 45 machine-level tests (`ui_test::tests`) + 22 pure
step/vars tests + 18 `sven-tools-ground` tests, all green, `cargo clippy
--all-targets -- -D warnings` clean for both new crates, workspace `cargo
check` clean. `cargo run -p xtask -- arch` reports no new violation from
this work (one pre-existing, unrelated `ARCH-007` on
`crates/bootstrap/src/task_tool.rs` predates this branch and was not
touched here).

Not yet done - real gaps, not swept under "mocked":

- **No real-device/real-checkpoint pass through the CLI entry point.**
  `crates/bootstrap/tests/live_ui_test_dispatch.rs` (added by this file's own
  CLI-wiring update below) self-skips cleanly without one, matching
  `tools-android/tests/live_device.rs`/`tools-ground/tests/live_ground.rs`'s
  existing convention - it did not run in this environment either.
- **"Verify" is just the acting tool call's own success/failure**, not an
  independent post-action re-screenshot/re-ground check the roadmap's
  original "screenshot -> ground -> act -> verify -> next step" phrasing
  could be read to imply. A `ToolSucceeded` on the `android`/`ask_question`
  call is what advances to the next step.
- **`launch_app`/`force_stop` take the compiled `target` as a literal
  Android package name** (no fuzzy app-name-to-package resolution via
  `list_packages`, e.g. "the demo app" is not resolved to
  `com.example.demoapp`). A test script must name the real package, or a
  future pass adds a list-then-launch resolution step.
- **`swipe` only supports the four cardinal directions** as fixed
  normalized coordinates, not an arbitrary described gesture.
- **No cancellation handling** (`Event::UserCancelled` is unhandled,
  matching `VerifiedTaskMachine`'s own scope, not an oversight specific to
  this machine).

### Reconciling with whale's Phase 4 (parallel work)

This was built while whale's Phase 4 (`NodeKind::Agent` dispatch,
device/resource leasing in `whale-marketplace::Catalog`) was still
unstarted, per that phase's own note. Nothing here assumes a particular
whale dispatch shape - `UiTestMachine` is seeded by one `Event::UserMessage`
carrying `{"steps": [...]}` JSON and reports its outcome entirely through
`Context` facts (`ui_test_results`, `ui_test_error`) plus the terminal
`Done`/`Failed` state, the same shape any `RuntimeBuilder`-constructed
kernel already exposes. When Phase 4 lands, reconcile on:

(a) **Who constructs the seed JSON.** `whale run ui-login.yaml` is a
*client*: it submits the workflow to a whale orchestrator (broker) and only
subscribes to events for rendering workflow/cluster state (terminal or web
UI) - it never constructs or serializes anything into a machine's seed
input itself. The orchestrator schedules the `NodeKind::Agent` task onto a
worker that has the needed device (`Catalog` gaining a device/resource
dimension is what makes that placement possible); it is that **worker's**
own `NodeKind::Agent` dispatch adapter that builds whatever seed/invocation
input `UiTestMachine` needs - an entirely worker-local dispatch detail, not
something the CLI client or the orchestrator does.

(b) Whether whale wants per-step progress before the terminal state (today
only the final `ui_test_results` fact has step-by-step detail; there is no
incremental `SessionEvent` per step).

(c) **Exclusivity is two-tier, and neither tier lives here.** The
orchestrator does capability-aware *placement* only - never double-booking
routing the same device across two concurrent assignments where avoidable
- while true hard exclusivity (never literally running two jobs against the
same physical phone at once) is enforced locally by whatever worker owns
that device, not by any distributed lock `UiTestMachine` or the
orchestrator holds. `UiTestMachine`'s own single-device assumption is still
fine as-is: a worker only ever hands it one device at a time (the same
assumption `sven-tools-android`'s `SVEN_ANDROID_SERIAL`/auto-detect already
makes) - it is simply not this machine's job to enforce exclusivity itself.

### Phase 3 update - wired into `mode.rs`/`RuntimeBuilder` via `sven agent-dispatch` (this session)

Closes the "not wired into `mode.rs`/`RuntimeBuilder`" gap named above and
by Phase 4's own reconciliation note (a): `UiTestMachine` now has a real CLI
entry point that speaks whale's real agent-dispatch stdio contract exactly
(`whale_workflow_runner::agent_dispatch`'s own doc, cross-checked against
`SubprocessAgentDispatcher`'s tests and `examples/ui-test/ui-login.yaml`/
`node-types.json` in the whale repo).

**The subcommand: `sven agent-dispatch`.** No arguments; one process per
node dispatch, exactly as `sh -c "<command>"` invokes it. Reads one JSON
object from stdin, then stdin is closed:

```json
{"mode": "ui-test", "device": {"provider_id": "local", "device_id": "phone-1"} | null, "params": {"instruction": "Launch the demo app", ...}}
```

- `mode` is branched on; only `"ui-test"` is handled - any other value
  writes `{"ok": false, "error": "unsupported mode: <mode>"}` and exits 0
  (a clean, well-formed refusal, not a crash).
- `device.device_id` (when present) selects the real ADB serial, exactly
  like `AndroidTool`'s own `SVEN_ANDROID_SERIAL` env var; falls back to that
  env var, then auto-detection, when `device` is `null`.
- `params.instruction` becomes the machine's one-element step list (this
  subcommand handles exactly one instruction per invocation, matching
  whale's per-node dispatch granularity). Every OTHER top-level `params`
  field is seeded into `UiTestMachine`'s existing variable-binding mechanism
  (`vars.rs`, Phase 3's own binding store) before the step compiles - a new
  `UiTestScript.vars` field, bound in `Seeding` via the same `vars::bind`
  Phase 3 already built for an in-run `ask_user` answer. This is the whole
  mechanism by which a resolved upstream `Link` value (whale's own
  `params.<name>` merge, documented in Phase 4's own update above) reaches
  this step's `value_ref` resolution - no new sven-side plumbing. Non-string
  JSON values are serialized to their JSON text rather than dropped, since a
  Link's resolved value can be any JSON type.

Stdout: exactly one JSON reply as the LAST line -
`{"ok": true, "output": {...}}` on success, `{"ok": false, "error": "..."}`
on failure (both a genuine setup failure and an ordinary failed UI-test step
that exhausted its retry budget). `output` always carries `{"passed": true,
"step": <the one ui_test_results entry>}`; if the step was itself an
`ask_user` step that named a `bind` variable, its answer is ALSO a
top-level, clearly-named field (e.g. `output.code`) - not buried in `step`
- so a later whale node's `Link` can read it directly by name, matching
Phase 4's own documented Link-resolution contract
(`Outcome.outputs` keyed by name). Exit code 0 covers both `ok` values; a
non-zero exit is reserved for a genuine subcommand-level fault (malformed
stdin, or an internal error building/joining the kernel session).

**How it's built** (`crates/bootstrap/src/ui_test_dispatch.rs`,
`dispatch_ui_test_step`): the SAME `RuntimeBuilder`/`ModeRegistry` path
every other sven machine uses - `mode.rs::default_registry()` now registers
`"ui-test"` → `UiTestMachine`, and `RuntimeBuilder::build()`'s permission-
policy match now has a `"ui-test" => UiTestMachine::permission_policy()`
arm (it previously fell through to the reactive-agent default, which is
wrong for this machine). Tools are wired via `RuntimeBuilder::
with_tool_executor_override` - the real `sven-tools-android::AndroidTool`
(device-selected per above), `sven-tools-ground::GroundTool` (unchanged,
brain's real `ground` capability + its FLAG_SECURE local detection), and
`sven-tools-agent::AskQuestionTool::new_headless()` for `UiTestMachine`'s
OWN internal `ask_user`/FLAG_SECURE hand-off - untouched by this change,
still the mechanism for sven's standalone/local multi-step runs. Since
nothing outside this one-shot process is listening on the kernel's human-
answer channel, it is auto-approved exactly like every other headless sven
surface already does (`sven_ci::RuntimeRunner` spawns the identical
`auto_approve` for CI runs) - a real per-node whale dispatch is not
expected to hit this path at all (a workflow author routes anything needing
literal human entry to a separate graph-level node instead, per this file's
own FLAG_SECURE constraint and Phase 4 update's reverted human-in-the-loop
attempt), but a step whose compiler genuinely resolves to `ask_user` still
completes rather than hanging forever with no answerer.

**Tests** (all against fakes/mocks, TDD'd red-then-green; no real device or
checkpoint needed for the default `cargo test` run):
- `crates/machines/src/machines/ui_test/mod.rs` - vars-seeding-from-script
  and `ask_user_binding` accessor tests (the `ui_test` module's full test
  suite, machine + step compiler + vars, is 50 tests).
- `crates/machines/src/mode.rs` - `"ui-test"` registry test.
- `crates/tool-registry/src/registry.rs` - `register_arc` tests (the seam
  that lets a fake tool double be substituted by `Arc` rather than only a
  concrete `impl Tool`).
- `crates/bootstrap/src/ui_test_dispatch.rs` - 7 tests: success/output
  shape, device-field plumbing, params-to-vars seeding (string and
  non-string), the ask_user-answer-is-a-named-output-field contract, and a
  retry-budget-exhausted failure.
- `src/run/agent_dispatch.rs` - 5 tests: stdin request parsing (with/without
  a device, with/without `params`, malformed JSON, missing `mode`).
- `crates/bootstrap/tests/live_ui_test_dispatch.rs` - the real-device/real-
  checkpoint integration test this file's own "not yet done" list above
  points at; self-skips cleanly without both, matching
  `live_ground.rs`/`live_device.rs`'s convention. Did not run in this
  environment (no ADB device attached).

All new/changed code is `cargo clippy --all-targets -- -D warnings` clean;
`cargo run -p xtask -- arch` reports no NEW violation from this work (the
one pre-existing `ARCH-007` on `crates/bootstrap/src/task_tool.rs` predates
this session and was confirmed unrelated - it fails identically on an
unmodified checkout).

**Still open:** whale's own dispatch path has not been re-pointed at
`sven agent-dispatch` (it still runs against `agent-dispatch-smoke.sh`, the
deliberately trivial acknowledge-and-reply script Phase 4 documents) - that
re-pointing is a whale-side `WHALE_AGENT_DISPATCH_CMD` configuration change,
not a sven-side gap. Per-step progress (Phase 4's own open item (b)) is
still unaddressed - this subcommand reports only the terminal outcome of
its one step, matching what `UiTestMachine` itself exposes today.

## Phase 4 - done for the local/single-worker case; not yet integration-tested against a real sven UiTestMachine (whale)

`NodeKind::Agent { mode }` (`whale-nodespec`) now has a real dispatch path,
`whale-marketplace::Catalog` now has a device/resource dimension used by
real placement logic, exclusive per-device leasing is implemented and
tested (including under real concurrent contention), and `whale run
ui-login.yaml --local` runs end to end against a real (generic,
honestly-labelled) dispatcher subprocess - verified by actually running the
built `whale` binary, not just `cargo test`. All work landed as
self-contained, TDD'd commits on whale's `main`.

**What's built, matched against Phase 3's own reconciliation note above:**

(a) **Who constructs the seed/invocation input.** `whale-workflow-runner::
agent_dispatch::AgentDispatcher` is exactly the worker-local dispatch
adapter that reconciliation note calls for: one async trait,
`dispatch(mode, device, params) -> Result<Value, String>`, no
`UiTestMachine`/ADB/sven type anywhere in its signature. The one shipped
implementation, `SubprocessAgentDispatcher`, spawns a configured command and
speaks a small generic JSON-over-stdio contract (`{"mode","device",
"params"}` in, `{"ok":true,"output":...}`/`{"ok":false,"error":...}` out) -
deliberately NOT a fake `UiTestMachine` API. A future commit wiring the real
`UiTestMachine` (via `sven_bootstrap::RuntimeBuilder`, once it has a mode
registry entry - see this file's own Phase 3 "not wired into
`mode.rs`/`RuntimeBuilder`" gap) writes a new `AgentDispatcher`
implementation; nothing in whale's dispatch path changes to use it.
`whale run ui-login.yaml` (no `--local`) is still the client-submits-
to-a-broker-and-only-subscribes path this note describes, and it is
genuinely untouched - `admin_submit.rs`/`node_cmd.rs` (the two places a
node executes a job on a remote submitter's behalf) still pass `agent:
None` exactly as before this phase, never constructing anything
agent/device-shaped. `--local` is the one whale mode where "client" and
"worker" are the same process (see whale's own `AGENTS.md`: "the same
engine runs the graph in-process against this machine's own brain
service") - its CLI-level `crate::agent_runtime` module (library code,
not `main.rs`, mirroring `crate::registry_build`'s own real-vs-mock
precedent) is that process building its own local worker config, not the
client reaching into a remote worker's internals.

(b) **Per-step progress.** Not addressed - genuinely open. Dispatch emits
one `NodeStarted`/`NodeCompleted`/`NodeFailed` per GRAPH NODE, not per
`UiTestMachine` step; a real integration would need either
`UiTestMachine` to report incremental progress through
`AgentDispatcher`'s existing (currently unused for this) progress
channel, or accept node-level granularity as sufficient for v1.

(c) **Exclusivity is two-tier**, both tiers now real and separately tested:
worker tier is `whale_marketplace::leasing::DeviceLeases` - a plain
`Mutex`-guarded set, in-process, no distributed lock, proven under real
concurrent contention (16 threads racing for the same key, never more than
one holder). Orchestrator tier is `whale_marketplace::HeadroomFirst`'s new
`place_one_device` path: `WorkloadNode::requires_device` routes a
device-needing node only to a provider whose `Catalog` reports a matching
device, ranked by the same headroom rule a capability node gets - explicitly
best-effort placement, no reservation, matching this crate's own
long-standing "no reservation, and no model of consumption" posture. Wiring
a REAL graph's device requirements into that placement path (today only the
placement ALGORITHM is real and tested; nothing in `crates/whale`'s
dry-run/broker code populates `requires_device` from a resolved
`NodeTypeMapping::device` yet) is the next real gap in this tier.

**Honest gaps, not swept under "done":**

- **Not integration-tested against a real sven `UiTestMachine`.** Phase 3's
  own machine has no CLI entry point yet (its own "not wired into
  `mode.rs`/`RuntimeBuilder`" gap, still open) - there is nothing running
  yet for a real `AgentDispatcher` implementation to invoke, so this
  integration genuinely has not happened end to end. `examples/ui-test/
  ui-login.yaml` in the whale repo runs against
  `scripts/dev/agent-dispatch-smoke.sh`, a deliberately trivial
  acknowledge-and-reply script - proof the DISPATCH PATH works, not proof
  real UI automation works.
- **Per-step progress** (b) is unaddressed.
- **No cross-machine distributed demo.** Everything above is verified
  through `whale run --local` (one process, one machine) plus
  `whale-marketplace`'s own unit/integration tests (pure algorithm, no
  network). Nothing here stands up a real multi-worker cluster - correctly
  out of scope per this phase's own original "not worth building scheduling
  scaffolding around a loop that hasn't been proven end to end yet".

### Phase 4 update - Link data-flow into Agent dispatch + real-graph device placement (whale, later session)

Closes two of Phase 4's own named gaps: `Link`-kind inputs used to be
silently dropped for `NodeKind::Agent` nodes (the capability path already
resolved them), and nothing in `crates/whale`'s real graph-loading path
populated `WorkloadNode::requires_device` from a resolved
`NodeTypeMapping::device` - the placement ALGORITHM was real and tested,
but never fed from a real graph. Both landed as TDD'd, self-contained
commits on whale's `main`; the second (device-loader) is tracked as whale's
own task #17.

**Link data-flow into Agent dispatch:**

- `whale_workflow::machine::WhaleWorkflowMachine::call_tool_effect_agent`
  now builds `args.blob_refs` for a `Link` input exactly the way the
  capability path already does (mirroring `call_tool_effect_capability`'s
  own `Link` arm, including fan-out `item` marking) - present only when at
  least one `Link` input exists, omitted entirely otherwise (the same
  "absent means did not say" discipline `args.device` already holds).
- The store lookup / fan-out item / `collect`-merge logic that used to live
  only inline in `BrainCapabilityExecutor::execute` is now a shared
  function, `whale_workflow_runner::call_args::resolve_link`, called by
  BOTH `BrainCapabilityExecutor` (capability path) and the new
  `AgentCapabilityExecutor` link-resolution step (agent path) - one lookup
  implementation, two different policies for what a resolved value means.
- `AgentCapabilityExecutor` now shares the SAME `NodeOutputStore`/
  `ItemOutputStore` pair `BrainCapabilityExecutor` writes to and reads
  `Link`s against (constructed once by `run_workflow_value`, cloned into
  both executors before either owns it) - a successful agent dispatch's
  whole JSON `output` is wrapped as `capability::Outcome { outputs: output,
  blobs: {} }` (an agent dispatch never produces a binary blob today) and
  written into that shared store via the SAME `crate::node_finish::finish_success`
  the capability path uses, so a LATER agent node's `Link` input can read an
  EARLIER agent (or capability) node's result exactly the way a capability
  node's `Link` already could.

**The exact contract - how a downstream agent node receives a resolved
upstream value in its `params`** (this is what a future sven-side CLI
subcommand consuming this needs to match):

1. A graph author writes an ordinary `Link` input on an `Agent`-kind node,
   e.g. `{in: link, node: "<upstream node id>", output: "answer"}` under
   input name `code` (or any name; renamed per
   `NodeTypeMapping::param_rename` exactly like a `Value` input already is)
   - this is illustrative, not naming any node in the shipped example
   graph, which stays two nodes (see below).
2. At dispatch time, whale looks at the upstream node's `capability::Outcome`
   (the same `Outcome` an agent dispatch's own successful result became, per
   above): if `Outcome.outputs` (a JSON object) has a key matching the
   Link's `output` name (`"answer"` in the example), THAT JSON VALUE is
   merged into the downstream node's dispatch `params` under the Link's
   (renamed) input name (`code`), UNCHANGED - any JSON type, not coerced to
   a string. This is the expected path for an agent-to-agent link: an
   upstream agent dispatch's own named result (e.g. an `ask_user` step's
   answer), or a capability node's scalar output mirror.
3. Only if `Outcome.outputs` has nothing under that name does whale fall
   back to `Outcome.blobs` (a capability node's binary output channel, e.g.
   a text blob): if a blob exists under that name AND is tagged
   `Media::Text`, its bytes are UTF-8-decoded into a JSON string and merged
   into `params` the same way. Any other blob media is refused (the node
   fails with a descriptive error) rather than silently guessed at - an
   agent dispatch's `params` is nowhere to smuggle raw bytes through.
4. If neither yields anything, the node fails with `"node '<id>' input
   '<name>' references unresolved output '<dep>.<output>'"` - the same
   never-fabricate posture every other link-resolution path in whale holds.

So: **a downstream agent node's dispatched `params` object gains one entry
per resolved `Link` input, keyed by that input's (renamed) name, valued by
the upstream node's own named JSON output verbatim** - e.g. if an upstream
node's `AgentDispatcher::dispatch` call returns `Ok(json!({"answer":
"1234"}))`, a downstream node Linking `{node: "<id>", output: "answer"}`
under its own input `code` receives `params.code == "1234"` (a JSON
string, not wrapped) in its own dispatch call. Proven end to end (real
dispatched `params`, not just an intermediate `Effect`) by
`whale_workflow_runner::agent_executor::tests::
a_link_input_on_an_agent_node_carries_the_upstream_agent_nodes_resolved_value`.

`examples/ui-test/ui-login.yaml` stays the small two-node
(`launch`/`login`) dispatch-path demo it already was - `login`'s `after`
input is a real value-carrying `Link` to `launch`'s own output (proof this
mechanism is live in the shipped example, not just under `cargo test`), but
a realistic multi-step login+add-card flow is deliberately NOT built into
this file yet. An earlier pass in this same session expanded it to a
five-node flow that relayed an `ask_user` confirmation-code answer into an
automated "type the code" step - correctly reverted: a secure confirmation
code has to be entered by the human on the device themselves, never
auto-typed by relaying an `ask_user` answer back through a `Link`, per
Phase 1's own `FLAG_SECURE` hand-off constraint. The right shape for a
"workflow blocks until a human physically acts" node is still being
designed (likely a distinct node kind, not an `ask_user`-relay-then-
automate pattern) - the full flow example is future work once that lands,
not scoped to this update.

**Real-graph device placement wiring:** `crates/whale/src/plan_cmd.rs`'s
`workload()` (reused by `dispatch::workload_from`, so both `whale run
--dry-run` and a broker's own placement decision go through it) now reads
each resolved `NodeTypeMapping` fully: an Agent-kind mapping
(`agent_mode.is_some()`) produces `WorkloadNode { requires: None,
requires_device: <mapping's device, converted into whale_marketplace's own
DeviceRequirement> }`; an ordinary capability mapping is the reverse. Proven
against the real loader (`dispatch::workload_from`, not a hand-built
`WorkloadNode`) loading the repo's actual `examples/ui-test/ui-login.yaml`
+ `node-types.json` through `crate::workflow_file::load`, the same path a
real `whale run`/`whale plan` invocation uses.

**Still open, unchanged by this update:** not integration-tested against a
real sven `UiTestMachine` (per above), per-step progress, and no
cross-machine distributed demo - none of these were in this update's scope.
