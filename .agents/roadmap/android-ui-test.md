# android-ui-test

**Status: phase 1 done (device control), phase 2 done (brain grounding),
phase 3 done against mocks (sven machine + tools; no real-device/real-
checkpoint pass yet), phase 4 not started.**

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

- **No real-device/real-checkpoint pass.** Nothing in this phase has been
  run against an actual attached device or a live `brain/florence2`
  checkpoint end to end (`sven-tools-ground/tests/live_ground.rs` self-skips
  cleanly without one, matching `tools-android/tests/live_device.rs`'s
  existing convention - it did not run in this environment either).
- **Not wired into `mode.rs`/`RuntimeBuilder`.** There is no `--mode
  ui-test` registry entry or CLI verb to actually launch this machine
  today; nothing in sven currently constructs a `UiTestMachine`. This is
  deliberate scoping (nothing consumes it yet - Phase 4 is whale's
  dispatch path), not an oversight, but it means the machine cannot be
  driven end to end from any sven surface until either a CLI entry point
  or Phase 4's dispatch lands.
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

## Phase 4 - not started (whale)

Give `NodeKind::Agent { mode }` (already in `whale-nodespec`, currently
display-only, zero execution path - see `whale-agent.md`/`crate-graph.md`
in the whale repo) a real dispatch path in `whale-workflow`'s runner, add a
device/resource dimension to `whale-marketplace::Catalog` (today strictly
`(model, action)`-keyed, no way to express "node has an Android device with
app X installed"), and enable `whale run ui-login.yaml`. Exclusivity is
two-tier (see Phase 3's reconciliation note above): the orchestrator does
capability-aware placement only, routing a device-needing node's task to a
worker that declares the resource; true hard exclusivity is enforced
locally by whichever worker owns that device, not by an orchestrator-held
distributed lock. Deliberately last: not worth building scheduling
scaffolding around a loop that hasn't been proven end to end yet.
