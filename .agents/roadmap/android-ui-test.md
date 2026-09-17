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
gap), phase 5 done (whale): the "workflow blocks until a human physically
acts" node Phase 4's own update deliberately deferred is now real -
`NodeKind::HumanAction`, a resume RPC, `whale run --local`'s own
cross-invocation resume socket, and `ui-login.yaml`'s `confirm_code`
step - see Phase 5's own update below for the exact contract. Phase 6 (whale)
closes the LAST open gap this whole initiative was scoped around: a codified
`crates/whale/tests/live_ui_test_e2e.rs` now drives the REAL `sven
agent-dispatch` binary (not the smoke script) through the FULL flow -
`launch` -> `login` -> `confirm_code` (pause) -> `whale resume` from a
second process -> `get_started` -> `add_card` -> `run_completed`. Building
it surfaced and fixed a real, load-bearing bug (a resumed `HumanAction`
node's output was never written into the shared node-output store, so any
downstream node's `Link` past it could never resolve) - see Phase 6's own
update below for the exact contract, what was verified for real in this
sandbox (the whole flow against the generic smoke dispatcher, including the
fix), and what remains genuinely unverified (a real run against real
hardware/checkpoint, which this sandbox has neither of). Phase 7 fixes the
FIRST real bug the actual user hit running this for real: whale's own
catalog/leasing device id (e.g. `"phone-1"`) was being sent AND used as the
literal ADB `-s` serial, so every real dispatch failed with `adb: device
'phone-1' not found` even with a real phone attached. `whale_marketplace::
DeviceSpec` gained a separate `serial` field (whale-side), and sven's
`dispatch_ui_test_step` now resolves the effective serial through
`sven-tools-android`'s own device-selection with a bounded auto-detect
fallback (an exact `serial` match wins, a mismatched/absent one falls back
to the sole attached device with a logged warning, only genuine ambiguity
hard-fails) - see Phase 7's own update below for the exact contract, and
whale's own `.agents/roadmap/android-ui-test.md` update for the whale-side
half (`DeviceSpec::serial`, the dispatch JSON shape, `devices.json`, and a
new `scripts/dev/run-ui-test.sh` one-command runner). Phase 8 fixed the next
real bug behind it (an app-name HINT is not a package name: "Launch betalo
app" compiled to `betalo`, which `monkey -p` can never launch). Phase 9
closes Phase 8's own "left to do" and, with it, a contract defect the whole
series had been accumulating: sven is a generic agent SDK and now names no
particular orchestrator anywhere in its code or doc comments (51 mentions
across 11 files removed - there was never a Cargo dependency, only
vocabulary), and a dispatching host can now DECLARE which apps a device runs
(`UiTestDevice::apps`, `#[serde(default)]` on the wire) so a hint resolves
against that declaration - exactly, and with no `pm list packages` round
trip - before the device is ever asked. See Phase 9's own entry for the four
precedence decisions and what is left.**

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

## Phase 5 - done (whale): `NodeKind::HumanAction` + resume RPC

Closes the gap Phase 4's own update named and deliberately deferred: "the
right shape for a 'workflow blocks until a human physically acts' node is
still being designed... the full flow example is future work once that
lands." This is that node kind, its resolution mechanism, and the
`ui-login.yaml` step demonstrating it - a whale-only change, no sven-side
code touched. All work landed as TDD'd, self-contained commits on whale's
`main`, verified against the real `whale` binary (not only `cargo test`) in
this same session.

**Why a distinct node kind, not another `NodeKind::Agent` mode string.** A
secure code-entry screen (Phase 1's own `FLAG_SECURE` constraint) can never
be automated at all - no device lease, no subprocess, no grounding call, no
`AgentDispatcher`. This genuinely different lifecycle (dispatch nothing,
wait indefinitely, resolved by an external human action rather than a tool
call finishing) earned its own `whale_nodespec::NodeKind::HumanAction`
rather than overloading `Agent`'s `mode` string with a magic value.

**How it dispatches, and why it is still an ordinary `Effect::CallTool`.**
sven-hsm already has a "park a question for a human, no timeout, resolved by
whoever gets to it" primitive
(`Effect::RequestHumanAnswer`/`Event::HumanAnswered`, the same one
`sven_machines::loop_core` uses for `ask_question`) - reusing it was the
first design tried, and reverted: its fields (`question_id`, `call_id`,
`prompt`, `options`) carry no node identity at all, so an executor that only
sees the bare effect has no way to know WHICH node to report as waiting,
short of hiding the node id in the human-facing prompt text. Instead,
`WhaleWorkflowMachine::human_action_effect` dispatches an ordinary
`Effect::CallTool` marked `args.human_action: true`, carrying
`args.node_id`/`args.item` exactly like every other node kind already does -
`whale_workflow_runner::HumanActionExecutor` (the OUTERMOST executor layer,
ahead of the agent/capability layers, since a human-action node's `args` has
neither `agent_mode` nor `model`/`action`) checks for that marker, emits
`NodeStarted` + a new `WorkflowProtocolEvent::NodeWaitingForHuman`, and
returns WITHOUT ever posting a kernel event - no `Event::ToolSucceeded`, no
background task, no timeout armed. Resolution therefore reuses the exact
same `Event::ToolSucceeded`/`call_id_to_node` path every node kind already
resolves through, via the deterministic id
`whale_workflow::human_action_call_id(node_id, item)` - no second
correlation map, no sven-hsm `Event`/`Effect` change needed at all (which
would have meant a cross-repo change to this separate git dependency).

**The prompt lives on the graph node, not the node type** - an ordinary
`"prompt"` input (`InputValue::Value`), the same way `Agent`'s
`instruction` does, since one `HumanAction`-kind `class_type` (e.g.
`ConfirmOnDevice`) is meant to be reused by many graph nodes with different
prompts, not baked into the type once.

**New wire surface** (`crates/workflow` in whale):
`NodeStatus::WaitingForHuman`,
`WorkflowProtocolEvent::NodeWaitingForHuman { seq, at_ms, node_id, prompt,
item }`, `NodeTypeMapping::human_action: bool` (projected to/from
`whale_nodespec::NodeKind::HumanAction`), `RunSnapshot::from_events` folding
the new event into `NodeStatus::WaitingForHuman`.

**The resume RPC - exact contract.** Two independent resume paths, both
resolving through the same underlying mechanism (posting
`sven_hsm::Event::ToolSucceeded` for the node's deterministic call id,
directly into the run's own live kernel `EventSink`), because the two
execution shapes whale has (single-process `--local`, and a node/broker
process running a p2p- or tenant-submitted job) have no shared IPC surface
to route through otherwise:

1. **Admin/tenant RPC**, for a node- or broker-hosted run -
   `whale::admin::TenantRequest::ResumeNode`:
   ```rust
   TenantRequest::ResumeNode {
       job_id: uuid::Uuid,
       tenant_id: String,
       node_id: String,
       fan_out_item: Option<u32>,
       output: serde_json::Value,   // defaults to {} on the wire
   }
   ```
   answered by `TenantResponse::ResumeNodeResult { job_id }` (posted - not a
   guarantee the node was actually waiting) or
   `TenantResponse::ResumeNodeUnavailable { job_id }` (unknown job id, a
   different tenant's job, or a job whose run has no live kernel to post
   into - all three deliberately indistinguishable to the caller, the same
   identity-scoping posture `RunSnapshot` already holds). Identity-scoped via
   the SAME `JobRegistry::tenant_events_for` ownership check `RunSnapshot`
   uses. Reaches the run via a new `JobRegistry::register_resume_sink`/
   `resume_sink` pair, populated the moment `run_workflow_value`'s kernel
   exists (mirroring the existing `cancel_token` bookkeeping). `whale-web`
   exposes this as `POST /api/workflows/[workflowId]/runs/[runId]/resume`
   (server-resolves the tenant from the Clerk session, never a
   client-supplied one) plus a "Waiting on you" / "Mark as done" state in the
   run dashboard.

2. **`whale run --local`'s own cross-invocation resume** (single-process, no
   admin socket at all) - a Unix domain socket at a well-known,
   `run_id`-keyed path (`whale::resume_local::local_resume_socket_path`),
   bound the moment the run's kernel exists, owner-only permissions
   (`0o600`). One JSON line in (`{"node_id": "...", "output": <value>}`),
   one JSON line out (`{"ok": true}` or `{"ok": false, "error": "..."}`).
   CLI usage, from a SECOND, separate `whale` invocation while the first is
   still blocked:
   ```
   whale resume <run-id-or-socket-path> --node <node-id> [--output '<json>']
   ```
   `<run-id-or-socket-path>` accepts either the run's own id (parsed as a
   UUID, then resolved to the same socket path `--local` derived) or an
   explicit socket path. `--output` defaults to `{}` - see below for why an
   empty object is the correct default, not a missing feature.

   Verified genuinely end to end against the real built `whale` binary in
   this session: `whale run ui-login.yaml --local ...` blocks at
   `confirm_code` (prints `node_waiting_for_human` then nothing further);
   `whale resume <run-id> --node confirm_code` from a second terminal
   resolves it; the first terminal's own process then prints
   `confirm_code`'s `node_completed` and `run_completed{status:"succeeded"}`,
   exit code 0.

**Deliberately not modeled: relaying a typed value back for automation.**
`HumanAction`'s `output` (both RPC paths) exists so a resume caller CAN
attach a JSON value, but the node's job is "a human did something on the
device," never "an automated value a downstream node consumes" - unlike
Phase 4's own reverted `ask_user`-relay-then-automate attempt for the
confirmation code, `confirm_code` in `ui-login.yaml` has no downstream
consumer of its output at all. This is a deliberate scope boundary, not an
oversight: a secure code a human enters is exactly the value whale must
never carry through the graph.

**Two real bugs this session's own live verification caught, that the unit
tests alone had not** (both fixed, both now covered by their own tests -
see whale's own commit history for the full detail):

1. A resumed `HumanAction` node's `NodeCompleted` protocol event was never
   emitted - `Event::ToolSucceeded` resolves the node inside the kernel, but
   nothing else was posting the matching outward `NodeCompleted` (that
   normally comes from the executor that made the call, and a human-action
   resolution posts directly into the kernel from OUTSIDE any executor).
   `whale-web`'s dashboard, folded from exactly that event stream, would
   have shown the node stuck at "waiting on you" forever even after the run
   actually finished. Fixed with `whale_workflow_runner::ResumeHandle`
   (bundles the kernel `EventSink` with the run's own protocol-event sender
   and start instant) - a resume caller now emits `NodeCompleted` itself,
   through the same seq-numbered stream, before posting the kernel event.
2. That fix's first version introduced a worse bug: storing a STRONG clone
   of the protocol-event sender inside `JobRegistry` (which deliberately
   outlives a finished job by `COMPLETED_JOB_TTL`) kept the run's own
   protocol channel open forever, silently deadlocking the run's own
   shutdown (`run_workflow_value` never returned, so a resumed job's status
   never went terminal). Fixed with a weak-sender variant
   (`whale_workflow_runner::WeakSeqEmitter`) that can still send while the
   run is genuinely alive but never itself extends that lifetime.

**Honest gaps, not swept under "done":**

- **Distributed subjob relay cannot resume a `HumanAction` node.** A
  placement spanning more than one provider (`Dispatch::RunDistributed`)
  relays each graph node to whichever peer runs it; that peer's own
  `HumanAction` pause lives on ITS OWN kernel, not reachable from the
  orchestrating node's admin socket. `crate::remote_executor` forwards the
  `NodeWaitingForHuman` event upward for visibility, but `ResumeNode` posted
  to the orchestrating node cannot currently reach that remote peer's own
  live run. Real, tracked, not solved by this phase.
- **No fan-out `HumanAction` node is tested.** `fan_out_item`/`item`
  parameters exist end to end (machine, executor, both resume paths), but
  nothing exercises a `HumanAction` node under `each`/`content_set` - the
  shipped example and this phase's own tests are all single-item.
- **`ResumeNodeResult`/a socket `{"ok": true}` means "posted," never
  "resolved a real waiting node."** An unknown or already-resolved node id
  posts (and is silently ignored by the kernel) exactly the same way a real
  one does, by design - a caller that wants to confirm resolution watches
  the run's own event stream for the matching `NodeCompleted`. This is a
  documented API property, not a race condition to fix.
- **Not integration-tested against a real sven `UiTestMachine`** (unchanged
  from Phase 3/4 - still genuinely open, and orthogonal to this phase: a
  `HumanAction` node never invokes `AgentDispatcher`/`sven agent-dispatch`
  at all).

## Phase 6 - closes the loop: `sven agent-dispatch` for real, the full flow, one bug found and fixed (whale)

This is the acceptance bar the whole initiative was scoped around: whale
dispatching to the REAL `sven agent-dispatch` binary (not
`agent-dispatch-smoke.sh`), driving a real device through the full flow -
`launch` -> `login` -> `confirm_code` (pause) -> resumed from a second
process -> `get_started` -> `add_card` -> `run_completed` - proven together
rather than as three separately-tested pieces. All work landed on whale's
`main` as TDD'd, self-contained commits.

**The example graph now encodes the full flow, not just the pause/resume
half.** `examples/ui-test/ui-login.yaml` (whale repo) gained two more
`UiTestStep` nodes after `confirm_code`: `get_started` (reaching the
post-login landing screen) and `add_card` (clicking through to INITIATE the
add-card flow). Deliberately stops at initiating, not completing, add-card -
typing a real card number/CVV is its own sensitive-entry concern, same
reasoning Phase 1's own `FLAG_SECURE` constraint already established for the
login code: automation never types financial data a human is meant to enter
themselves. The step text is a plausible, generic guess at the real app's
post-login screens (this initiative's own Phase 1 manual walkthrough
confirmed a real login + add-card flow exists, but did not record exact
button copy) - what the graph exists to prove is the STRUCTURE (automated,
automated, human pause, automated, automated, all against the SAME leased
device, in order) and the dispatch/leasing/Link/resume machinery underneath
it, not a pixel-perfect script of the target app's real UI.

**A real bug this extension surfaced, found and fixed the same way every
other stage of this initiative has:** every structural `after` Link in the
graph reads its upstream step's `passed` output - but a real `sven
agent-dispatch` reply's own shape is `{"passed": true, "step": {...}}` (see
`dispatch_ui_test_step`'s own doc, Phase 3's update above), which has no
`note` field at all. The graph's Links used to read `output: note` (the
smoke dispatcher's own fixed acknowledgement key) - harmless against the
smoke script, but a silent, genuine break against the real dispatcher:
`login`'s own `after` Link would have failed to resolve with an
unresolved-output error the very first time this graph ran for real. Fixed
by renaming every Link's `output` to `passed` and updating
`agent-dispatch-smoke.sh` (whale repo) to also emit `"passed": true`
alongside its existing `"note"` - both dispatchers now agree on the field a
structural Link actually reads.

**A second, deeper real bug: a resumed `HumanAction` node's output was never
visible to a downstream `Link` at all.** `get_started`'s own `after` Link
reads `confirm_code`'s `passed` output - but `whale::resume_local`'s resume
handler only ever posted the kernel event and emitted `NodeCompleted`; it
never wrote the resumed value into the shared node-output store every OTHER
node kind's success path writes into (`whale_workflow_runner::node_finish::
finish_success`). A downstream Link past a resumed `HumanAction` node would
therefore ALWAYS fail, no matter what `--output` a human supplied. Fixed
with `whale_workflow_runner::ResumeHandle` gaining two new `Option` fields
(`outputs`/`item_outputs`, the SAME shared stores every other executor
writes into) and a small relay task inside `run_workflow_value` that
enriches the handle with the run's real stores before handing it to a
resume caller - `run_workflow_with_executor`'s own public signature (which
has other direct callers, including the p2p node-hosted path and several
tests) is completely unchanged; only the ONE path that owns the real stores
now threads them through. `whale::resume_local::handle_resume_connection`
then writes the resumed `output` into that store, exactly like
`finish_success` does for every other node kind, before posting the kernel
event. Scoped deliberately narrow: the p2p/tenant-hosted resume paths
(`admin_submit.rs`, `node_cmd.rs`) do NOT get this enrichment in this pass -
a known, tracked, honestly-scoped gap (they never had it before either, so
this is not a regression), not silently left inconsistent.

This fix does not reverse the earlier, deliberate decision that a
`HumanAction` node must never auto-relay a secret typed on the device: the
value written is exactly, and only, whatever JSON an OPERATOR explicitly
attached via `whale resume ... --output '<json>'` (defaulting to `{}` if
they attach nothing) - never anything auto-captured from the screen. Nothing
about what CAN enter the system changed; this only makes an already-optional,
already-caller-supplied field usable by a following node, the same as every
other node kind already allows.

**The acceptance test**: `crates/whale/tests/live_ui_test_e2e.rs` (whale
repo). Self-skips cleanly (matching `live_device.rs`/`live_ground.rs`/
`live_ui_test_dispatch.rs`'s own convention exactly) unless a real, built
`sven` binary (`SVEN_BIN` env var, never a hardcoded path), a single ready
ADB device, and a real florence2 checkpoint (`BRAIN_FLORENCE2_DIR`) are ALL
present. When they are, it spawns the real `whale` binary running the real
example graph with `WHALE_AGENT_DISPATCH_CMD` pointed at `"$SVEN_BIN
agent-dispatch"`, waits (bounded, never indefinitely) for `run_started` then
`node_waiting_for_human` on `confirm_code`, resumes it from a SEPARATE
`whale resume` process with `--output '{"passed": true}'` (required for
THIS graph, since `get_started` Links to it), waits for `run_completed` with
`status: succeeded`, and asserts the original process exits 0 - with a
`RunProcessGuard` that force-kills the child on any failure path so a device
lease or a hung process is never left behind between test runs.

**What was actually verified, and what was not (read this precisely):**

- Verified for real, in this sandbox: the ENTIRE mechanical flow - all five
  nodes, the pause, the cross-process resume, the `passed`-field fix, and
  the `NodeOutputStore` fix - end to end against the real `whale` binary and
  the smoke-test dispatcher (`whale run` printed `run_completed`
  `{"status":"succeeded"}`, exit code 0, `get_started`/`add_card` both
  genuinely dispatched after resume). `whale-workflow-runner`'s and
  `whale`'s own test suites: 480 + 18 + integration tests green (1544
  passed, only the same 6 pre-existing, unrelated failures -
  `admin_boundary`/`distributed_execution`/`portal_marketplace_execution`/
  `pricing` - that predate this work), `cargo clippy -D warnings` clean on
  the touched crates, `cargo run -p xtask -- arch` clean, the repo's own
  SPDX/no-machine-paths/no-doc-citation gates clean.
- NOT verified: `live_ui_test_e2e.rs` was never run against a real Android
  device or a real florence2 checkpoint - this sandbox has neither. It was
  confirmed to compile and self-skip cleanly (the correct, expected
  behaviour here), but the real-hardware pass this whole initiative was
  ultimately scoped around still needs to happen on a box with an actual
  device attached and a real checkpoint downloaded. Say this plainly rather
  than claiming a pass that did not happen.

**Honest gaps, not swept under "done":**

- The p2p/tenant-hosted resume paths still do not enrich `ResumeHandle` with
  the real output stores (see the bug-fix note above) - a `HumanAction` node
  resumed through `TenantRequest::ResumeNode` still cannot feed a downstream
  `Link`. Tracked, not solved here (out of this phase's scope: only
  `whale run --local` needed it for this acceptance test).
- The real-hardware run itself, per above.
- Per-step progress (Phase 4(b)) and a real multi-worker distributed demo
  (Phase 4's own "no cross-machine distributed demo") remain unaddressed,
  unchanged from earlier phases.

## Phase 7 - the real user's first real bug: device_id is not a serial (sven + whale)

The gap Phase 6's own "what remains genuinely unverified" note predicted -
a real run against real hardware - happened for the actual user, and it
failed immediately: `devices.json` names a device under the catalog key
`"phone-1"` (whale's own stable logical/leasing identity, used for
placement and exclusive-lease bookkeeping - never a physical address), but
that key was sent verbatim as `device.device_id` in the JSON whale
dispatches to `sven agent-dispatch`, and `dispatch_ui_test_step` used
`device.device_id` directly as `AndroidTool`'s default ADB serial. Real ADB
serials look like `ec677a50`, not `phone-1` - every real run failed with
`adb: device 'phone-1' not found`, even with a real device genuinely
attached and visible to `adb devices`.

**Fix, sven side (this repo):**

- `UiTestDevice` (`crates/bootstrap/src/ui_test_dispatch.rs`) gains a
  `serial: Option<String>` field, kept deliberately separate from
  `device_id` - a device can be re-plugged under the same logical role with
  a different physical unit over time, so the two identities are never
  conflated. `DispatchDevice` (`src/run/agent_dispatch.rs`, the `sven
  agent-dispatch` stdin parser) gains the matching optional `serial` field,
  `#[serde(default)]` so a request naming no serial at all still parses
  cleanly (a real, expected state, not a malformed request).
- New in `sven-tools-android` (`crates/tools-android/src/adb.rs`), reused
  rather than reimplemented by `dispatch_ui_test_step`:
  - `pick_serial(requested, ready) -> SerialPick` - the pure decision core.
    An exact match on `requested` wins outright; otherwise, when exactly
    one device is ready, it is used (silently if nothing was requested -
    ordinary auto-detect; as a named `FellBackToSole` substitution if
    something WAS requested but didn't match - a real fallback a caller
    should log); zero or two-or-more ready devices with no exact match is
    `NoneAttached`/`Ambiguous`, never guessed at.
  - `DeviceLister` trait + `RealDeviceLister` (shells to real `adb
    devices`) - the dependency-injection seam that lets
    `resolve_serial_validated` (and any caller of it) be unit-tested
    against a fixed, synthetic device list rather than a real `adb`
    invocation.
  - `resolve_serial_validated(lister, requested) -> Result<SerialPick, String>`
    - the async wrapper: fetches the ready device list via `lister`, then
      delegates to `pick_serial`. Deliberately a NEW entry point, not a
      behaviour change to the existing `resolve_serial` (which
      `AndroidTool`'s own interactive/CI callers already use and which
      several existing tests assert never shells out to `adb` when a
      default serial was given) - validating a caller-supplied identity
      against reality is the right default for whale's own catalog id, but
      would be a surprising, untested behaviour change for every other
      existing caller of `resolve_serial`.
- `dispatch_ui_test_step` gains `resolve_effective_serial(device, lister)`:
  `device: None` (no whale device info at all) preserves the exact
  pre-existing behaviour (fall back to `SVEN_ANDROID_SERIAL`, then
  `AndroidTool`'s own per-call auto-detect). `device: Some(d)` routes
  through `resolve_serial_validated(lister, d.serial.as_deref())` and, on a
  `FellBackToSole` substitution, logs a `tracing::warn!` naming both the
  catalog id and the requested/found serials (e.g. `"requested device
  'phone-1' (serial: none) not found; falling back to the only attached
  device 'ec677a50'"`) before proceeding - never a silent swap. Ambiguous/
  none-attached resolve to a descriptive `Err` naming the catalog id, for
  the same reason.
- `UiTestDispatchOverrides` gains `device_lister: Option<Arc<dyn
  DeviceLister>>` (defaults to the real one) - the seam the tests below use.

**Tests** (all against fakes; TDD'd red-then-green; no real `adb` shelled
out to in any unit test):

- `crates/tools-android/src/adb.rs` - 8 new tests: `pick_serial`'s exact-
  match/fallback/none-attached/ambiguous/ready-state-only cases (pure, no
  I/O), plus `resolve_serial_validated` against a fake `DeviceLister`
  (filters to ready before deciding; propagates a real lister failure).
- `crates/bootstrap/src/ui_test_dispatch.rs` - 7 new tests on
  `resolve_effective_serial` (a `PanicsIfCalled` lister proves the no-
  device-at-all path never even touches the lister; exact match; mismatched-
  falls-back; missing-serial-falls-back; zero-attached and two-with-no-match
  both hard-fail naming the catalog id; two-attached-with-an-exact-match
  still resolves) plus the existing device-plumbing test updated to inject
  a fake lister rather than hitting real `adb`.
- `crates/tools-android/tests/live_device.rs` - 2 NEW hardware-gated tests
  (`resolve_serial_validated_falls_back_to_the_real_sole_attached_device`,
  `resolve_serial_validated_uses_a_real_exact_match_directly`), same self-
  skip convention as the rest of that file.

**What was verified for real, and what was not (read this precisely):** a
real Android device (serial `ec677a50`) was transiently attached to this
sandbox during this session - both new `live_device.rs` tests were run
against it for real and passed, genuinely proving `resolve_serial_validated`
round-trips through a real `adb devices` call and substitutes correctly. The
device was no longer attached by the time this phase's work was committed,
so `cargo test --workspace` now self-skips that file cleanly, as designed -
confirmed by re-running it. NOT verified: the full whale -> `sven
agent-dispatch` -> real device path end to end with the actual bug's exact
`device_id`/no-`serial` `devices.json` shape - that needs `brain`/a real
florence2 checkpoint, neither present in this sandbox (see Phase 6's own
same caveat, unchanged). The device-selection fix itself is the part this
phase was scoped around, and that part - the actual reported bug - was
verified against real hardware, not just mocks.

`cargo test -p sven-tools-android -p sven-bootstrap -p sven` and `cargo
clippy --all-targets -- -D warnings` on the touched crates are clean;
`cargo run -p xtask -- arch` reports no new violation.

## Phase 8 - an app-name hint is not a package name

**Found by running the exact end-to-end path Phase 7 listed as NOT
verified**: whale -> `sven agent-dispatch` -> real device, with a real
florence2 checkpoint and `ec677a50` attached. Phase 7's serial fix worked -
the dispatch reached `adb` and ran a real `monkey` launch - and the step
immediately behind it failed:

    launch_app failed (is 'betalo' installed?):
      args: [-p, betalo, -c, android.intent.category.LAUNCHER, 1]

The device genuinely has the app installed, as `se.betalo.androidapp`.

**The defect is a self-contradictory contract.** `ui_test`'s step-compiler
prompt (`machines/src/machines/ui_test/step.rs`) tells the model
`launch_app/force_stop (target = app or package name hint)` - a HINT, in as
many words. `direct_action_effect` then passed that hint verbatim as
`package`, and `tools-android`'s `launch_app` handed it straight to
`monkey -p`, which needs an exact package name. So "Launch betalo app"
compiled to `betalo` and could never launch anything.

Structurally identical to Phase 7: an identifier from one naming world
(a human's app nickname) used directly as an address in another (Android's
package namespace), exactly as whale's catalog key was used as an ADB
serial.

**Fix** (`crates/tools-android/src/adb.rs`), mirroring
`pick_serial`/`resolve_serial_validated`'s shape deliberately:

- `PackagePick` = `Resolved` | `ResolvedFromHint` | `NoneInstalled` |
  `Ambiguous(Vec<String>)`, the package analogue of `SerialPick`.
- `pick_package(hint, installed)` - pure, no I/O. Precision tiers, first
  tier that matches anything decides: exact name; last dot-segment;
  any dot-segment (`betalo` -> `se.betalo.androidapp`); substring. Tiers
  2-4 case-insensitive. A tier matching 2+ packages is `Ambiguous`, never
  guessed at - same reasoning `pick_serial` applies to two attached phones.
- `PackageLister`/`RealPackageLister` + `resolve_package_validated`, the
  same fake-able seam `DeviceLister` already uses.
- `tool.rs::resolve_package_or_refuse` wires it into BOTH `launch_app` and
  `force_stop`, after the existing `valid_package_name` check (so the
  injection guard still runs first, on the raw hint). A `ResolvedFromHint`
  is logged at `warn` naming what it resolved to, never applied silently.

**Tests**: 10 new in `adb.rs`, TDD'd red-then-green (confirmed red: 20
compile errors before the implementation existed) - exact match untouched;
the real `betalo` -> `se.betalo.androidapp` case; case-insensitive;
last-segment; no-match-is-not-guessed-at; ambiguous names every candidate;
exact wins over competing loose matches; `pm list packages` parsing;
`resolve_package_validated` through a fake lister and its failure
propagation.

**Verified**: `cargo test -p sven-tools-android` fully green INCLUDING the
hardware-gated `live_device.rs` suite, which ran against the real attached
`ec677a50` in this session. `make check` clean (arch ratchet + clippy
`-D warnings`, both profiles).

## Phase 9 - sven is a generic agent SDK, and the app list is part of the contract

Two things, one theme: sven's own contract had grown the shape of ONE
caller, both in what it said and in what it left out.

### 9a - sven names no orchestrator, anywhere

sven is an agent SDK. It has no Cargo dependency on any orchestrator (and
never did - checked, not assumed), but 51 mentions of one particular caller
had accumulated across 11 Rust files, including in the PUBLIC doc comments
of `UiTestDevice`, `DispatchRequest`, `AndroidTool`'s device selection, the
`agent-dispatch` CLI help text, and - furthest from anything device-related
- `sven-memory`'s `FactSubmitter` trait. One `sven agent-dispatch --help`
example was literally a foreign tool's command line with a foreign env var.

That is a real defect, not a cosmetic one: a doc comment is the contract. A
type documented as "the device a <specific product> dispatch resolved" reads
as coupling to anyone evaluating sven as an SDK, and it quietly discourages
a second embedder from using the same seam.

Every one is now stated in role terms - "the dispatching host", "an
orchestrating host", "a caller's own catalog/leasing key", "a remote
`FactSubmitter`". Nothing about the architecture changed, because there was
nothing coupled to change; only the vocabulary, which was the whole problem.
`git grep -i` over `crates/` and `src/` is the standing check.

Fixed on the way past (stale since Phase 7, found while rewording):
`dispatch_ui_test_step`'s own doc still claimed `device_id` is "used exactly
like `AndroidTool`'s own `SVEN_ANDROID_SERIAL` env var" - the exact
conflation Phase 7 existed to end. It now describes `serial` resolution.

### 9b - a declared app list, threaded end to end

Phase 8's own "Left to do": a host that already knows which apps a device
runs had no way to say so, so every `launch_app`/`force_stop` hint was
resolved by sniffing `pm list packages` off the device.

The wire and the seam, both additive:

- `UiTestDevice::apps: Vec<String>` and `DispatchDevice::apps`
  (`#[serde(default)]`) - a request written before the field existed still
  parses, and an empty list is a real, expected state (a host that keeps no
  app inventory), not a malformed request.
- `AndroidTool::with_declared_packages(Vec<String>)` - a builder method, so
  `AndroidTool::new`'s existing signature is untouched.
- `adb::resolve_package_validated` gained a `declared: &[String]` tier that
  runs BEFORE the device is asked, via the same pure `pick_package` both
  tiers share.

The three decisions worth stating, because each could defensibly have gone
the other way:

- **A declared match skips the device entirely** - no `pm list packages`
  subprocess at all. This is what makes resolution *exact* rather than a
  guess: `betalo` against a one-app declaration cannot be derailed by an
  unrelated system package sharing a dot-segment.
- **A hint the declaration does not cover still falls back to the device.** A
  declaration names the apps a caller CARES about, not everything installed
  - a step driving the device's own Settings app must still work.
- **Ambiguity inside the declaration refuses rather than broadening.**
  Asking the device could only add candidates, never remove one.
- **A declaration is trusted, not re-verified.** A stale entry surfaces as
  the launch itself failing, which is cheaper and no less honest than a
  second round trip that can go stale just as fast.

**Tests**: 6 new, TDD'd red-then-green (confirmed red: 6 compile errors in
`adb.rs`, 2 in the wire parser, before either implementation existed).
Declared-resolves-without-any-I/O (a `PanicsIfCalled` lister proves the
device is never asked); uncovered-hint-falls-back; empty-declaration-behaves-
exactly-as-before; ambiguous-declaration-refuses; and the two wire cases
(`apps` parses, `apps` defaults to empty).

### 9c - `current_app` was broken on Android 15 (found by the gate, on real hardware)

Not planned, not related to 9a/9b - `make test` surfaced it because a real
device happened to be attached to this box while the gate ran, and
`live_device.rs` stopped self-skipping:

    could not determine foreground app (no topResumedActivity/
    mResumedActivity/mFocusedActivity/mCurrentFocus/mFocusedApp line
    in dumpsys output)

The device genuinely had a foreground app. Measured on the attached
Android 15 unit (`ec677a50`):

| probe | focus-marker lines |
|---|---|
| `dumpsys activity activities` | 0 |
| `dumpsys window windows` | 0 |
| `dumpsys window` | 2 (`mCurrentFocus`, `mFocusedApp`) |

Android 15 no longer reports the focus lines under the `windows`
sub-command, and `current_app` only ever tried the first two. `current_app`
now falls through to a bare `dumpsys window` as a third probe. The order is
deliberate: the narrower sub-command stays FIRST because its output is a
fraction of the size, and neither probe replaces the other - older devices
answer the first, Android 15 the second.

Verified by the previously-failing `current_app_reports_a_focus_line` now
passing against that same real device, plus the 2 new live package tests
(`resolve_package_validated_round_trips_through_a_real_device`, and
`a_declaration_outranks_what_the_real_device_reports` - which declares a
package that is genuinely NOT installed, so a declaration being ignored or
merely merged with `pm list packages` would fail it). Whole hardware-gated
suite: 11 passed, 0 failed.

### Left to do

- **The host side of 9b.** sven now accepts and uses `apps`; the orchestrator
  that dispatches to it must actually put its declared list into the
  request. That half is a change in the calling repo, not this one.
- **`sven-tools-ground` still shells out to a `brain` CLI by default.** The
  `GroundBackend` trait already makes that a swappable default rather than a
  hard dependency (this crate names no brain type), which is the right
  shape - but the default itself still assumes a specific binary on `PATH`,
  and the crate description still says so. Worth deciding whether the
  standalone default should instead be "no backend configured".
- **`.agents/roadmap/*.md` still names a specific orchestrator throughout**
  (3 files). 9a deliberately covered code and doc comments only; whether
  these internal planning notes should be reworded too, or left as accurate
  cross-repo history, is a call worth making explicitly.
