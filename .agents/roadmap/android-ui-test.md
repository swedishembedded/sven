# android-ui-test

**Status: phase 1 done (device control), phases 2-4 not started.**

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

## Phase 2 - not started (brain)

Add a `ground_ui`-shaped capability action returning normalized
`{found, bbox, score}` JSON (imitate `scrfd::caps`'s `detect` action, not
qwen3vl's free-text `generate`). Model choice deliberately NOT qwen3vl for
v1: this dev box has no GPU and is already memory-pressured (7.3 GiB free
of 30 GiB, swap in use) - a 4B VLM is a bad fit. Start with something in
the few-hundred-M-param range purpose-built for text-conditioned grounding,
added via the `brain:add-new-model` skill (full parity-gate discipline,
not a shortcut). Qwen3-VL stays a valid second/stronger grounder later,
hardware permitting.

## Phase 3 - not started (sven)

`UiTestMachine`: implements `Machine` directly. A step compiler turns one
natural-language instruction line (any language - the real test script
this was scoped against mixes English and Swedish) into a structured
`{verb, target, value | value_ref | ask_user}` via one bounded, schema-
constrained text-model call - not vision, just instruction-following.
Needs a variable-binding mechanism so `ask_user` results can be referenced
by a later step ("Ask the user for the code" -> "enter the code").

## Phase 4 - not started (whale)

Give `NodeKind::Agent { mode }` (already in `whale-nodespec`, currently
display-only, zero execution path - see `whale-agent.md`/`crate-graph.md`
in the whale repo) a real dispatch path in `whale-workflow`'s runner, add a
device/resource dimension to `whale-marketplace::Catalog` (today strictly
`(model, action)`-keyed, no way to express "node has an Android device with
app X installed"), exclusive per-device leasing, `whale run
ui-login.yaml`. Deliberately last: not worth building scheduling
scaffolding around a loop that hasn't been proven end to end yet.
