// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What [`super::UiTestMachine`] does at each phase of a single step: route a
//! freshly compiled step, work through the locating phases, and hold an
//! action to having changed something.
//!
//! Split out of `mod.rs` so the machine file stays the state table and the
//! per-phase decisions live together, where the reasoning about them reads
//! in one piece.
//!
//! Swedish Embedded AB implements solutions for deterministic on-device
//! Android UI testing for its clients. If your team needs expertise in
//! HSM-driven test automation, you can procure our services by sending an
//! email to info@swedishembedded.com.

use serde_json::{json, Value};
use sven_hsm::{context::Context, status::Reaction};

use super::{
    advance_or_finish, attempt_number, fail_or_retry, load_compiled, load_index, load_steps,
    set_pending, step, UiTestState, ASK_BIND_FACT, ASK_USER_OPTIONS, BASELINE_SIG_FACT,
    HANDOFF_OPTIONS, LOCATING_PHASE_FACT,
};
use step::CompiledStep;

/// Whether this verb's success means "the screen moved", and so must prove
/// it did.
///
/// Only the verbs whose entire purpose is to move the UI along. The rest
/// are exempt for concrete reasons, not for convenience:
///
/// - `launch_app`/`force_stop` state a GOAL, not a change: bringing an app
///   to the foreground when it is already there is a correct no-op, and
///   holding it to "the screen differs" would fail a run for doing exactly
///   what was asked. They carry their own postcondition regardless - adb
///   fails them outright on a package that cannot be resolved or started.
/// - `wait` exists precisely to change nothing.
/// - `ask_user` never touches the device.
fn needs_verification(verb: step::StepVerb) -> bool {
    use step::StepVerb;
    match verb {
        StepVerb::Tap | StepVerb::TypeText | StepVerb::Swipe | StepVerb::KeyEvent => true,
        StepVerb::LaunchApp | StepVerb::ForceStop | StepVerb::Wait | StepVerb::AskUser => false,
    }
}

/// After a successful action: re-read the hierarchy if this verb owes proof
/// that it changed something, otherwise just move on.
pub(super) fn begin_verification_or_finish(ctx: &mut Context) -> Reaction<UiTestState> {
    let Some(compiled) = load_compiled(ctx) else {
        return advance_or_finish(ctx);
    };
    if !needs_verification(compiled.verb) {
        return advance_or_finish(ctx);
    }
    // A step whose baseline was never recorded has nothing to compare
    // against; treating that as a pass is the old behaviour, and preferable
    // to failing a step for a bookkeeping gap.
    if ctx
        .fact(BASELINE_SIG_FACT)
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return advance_or_finish(ctx);
    }

    let index = load_index(ctx);
    let attempt = attempt_number(ctx, index);
    let (effect, call_id) = step::ui_signature_effect(index, attempt);
    set_pending(ctx, call_id);
    Reaction::transition(
        UiTestState::Verifying,
        vec![effect],
        "action reported success; checking it changed the screen",
    )
}

/// Compare the post-action hierarchy against the pre-action baseline.
///
/// An identical digest means the action was a no-op: the tool call
/// succeeded (`adb shell input tap` exits 0 for any coordinate on the
/// display) but nothing happened. That is a failed step, not a passed one.
pub(super) fn handle_verifying_success(
    ctx: &mut Context,
    observation: &Value,
) -> Reaction<UiTestState> {
    let after = match step::parse_signature(observation) {
        Ok(v) => v,
        Err(e) => return fail_or_retry(ctx, e),
    };
    let before = ctx
        .fact(BASELINE_SIG_FACT)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    if before == after {
        let index = load_index(ctx);
        let steps = load_steps(ctx);
        let instruction = steps.get(index as usize).map_or("", String::as_str);
        return fail_or_retry(
            ctx,
            format!("\"{instruction}\" left the screen unchanged, so it did nothing"),
        );
    }
    ctx.set_fact(BASELINE_SIG_FACT, Value::Null);
    advance_or_finish(ctx)
}

/// Route a freshly compiled step to whichever phase it needs: `Locating`
/// for `tap`, straight into `Acting` for everything else (`ask_user` builds
/// its own `CallTool` directly; every other verb goes through
/// `direct_action_effect`).
pub(super) fn dispatch_compiled(
    ctx: &mut Context,
    compiled: &CompiledStep,
    index: u32,
    attempt: u32,
) -> Reaction<UiTestState> {
    use step::StepVerb;

    match compiled.verb {
        StepVerb::Tap if compiled.target.is_none() => {
            fail_or_retry(ctx, "tap step compiled with no target")
        }
        // Every verb that drives the device goes through the same gate
        // first: refuse a FLAG_SECURE screen, then either resolve the tap
        // target or record the pre-action baseline.
        verb if needs_verification(verb) => {
            let (effect, call_id) = step::secure_check_effect(index, attempt);
            set_pending(ctx, call_id);
            ctx.set_fact(LOCATING_PHASE_FACT, "secure");
            Reaction::transition(
                UiTestState::Locating,
                vec![effect],
                "compiled a device-acting step; checking the screen is drivable",
            )
        }
        StepVerb::AskUser => {
            let question = compiled
                .target
                .clone()
                .unwrap_or_else(|| "Please help with this step.".to_string());
            let options = ASK_USER_OPTIONS.iter().map(|s| s.to_string()).collect();
            let (effect, call_id) = step::ask_user_effect(index, attempt, &question, options);
            set_pending(ctx, call_id);
            ctx.set_fact(ASK_BIND_FACT, json!(compiled.bind));
            Reaction::transition(
                UiTestState::Acting,
                vec![effect],
                "compiled an ask_user step",
            )
        }
        _ => match step::direct_action_effect(ctx, compiled, index, attempt) {
            Ok((effect, call_id)) => {
                set_pending(ctx, call_id);
                ctx.set_fact(ASK_BIND_FACT, Value::Null);
                Reaction::transition(
                    UiTestState::Acting,
                    vec![effect],
                    "compiled a direct action step",
                )
            }
            Err(reason) => fail_or_retry(ctx, reason),
        },
    }
}

/// Handle a `ToolSucceeded` while in `Locating`: either the screenshot just
/// came back (take the path, ground it) or the ground result just came back
/// (act on it, or hand off to a human on a `FLAG_SECURE` frame).
pub(super) fn handle_locating_success(
    ctx: &mut Context,
    observation: &Value,
) -> Reaction<UiTestState> {
    let index = load_index(ctx);
    let attempt = attempt_number(ctx, index);
    let phase = ctx
        .fact(LOCATING_PHASE_FACT)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    match phase.as_str() {
        // The FLAG_SECURE gate. A secure screen is one a human is meant to
        // handle; the accessibility tree stays readable on one, so without
        // this check nothing would stop a run driving it.
        "secure" => {
            let screen = match step::parse_secure_screen(observation) {
                Ok(v) => v,
                Err(e) => return fail_or_retry(ctx, e),
            };
            // An asleep device is recoverable by a human in one gesture, and
            // saying so beats the FLAG_SECURE hand-off a black frame would
            // otherwise trigger.
            if screen.display_off {
                return fail_or_retry(
                    ctx,
                    "the device display is off - wake and unlock it; a dark screen \
                     cannot be read or driven",
                );
            }
            if screen.secure_screen {
                let steps = load_steps(ctx);
                let instruction = steps.get(index as usize).map_or("", String::as_str);
                let question = format!(
                    "This step needs a human: the screen is marked FLAG_SECURE, so \
                     automation must not drive it. Original instruction: \"{instruction}\". \
                     Please complete it manually on the device, then confirm."
                );
                let options = HANDOFF_OPTIONS.iter().map(|s| s.to_string()).collect();
                let (effect, call_id) = step::ask_user_effect(index, attempt, &question, options);
                set_pending(ctx, call_id);
                ctx.set_fact(ASK_BIND_FACT, Value::Null);
                return Reaction::transition(
                    UiTestState::Acting,
                    vec![effect],
                    "secure screen detected; handing off to a human",
                );
            }

            let compiled = load_compiled(ctx);
            let is_tap = compiled
                .as_ref()
                .is_some_and(|c| c.verb == step::StepVerb::Tap);
            if is_tap {
                let target = compiled.and_then(|c| c.target).unwrap_or_default();
                let (effect, call_id) = step::find_element_effect(index, attempt, &target);
                set_pending(ctx, call_id);
                ctx.set_fact(LOCATING_PHASE_FACT, "locate");
                Reaction::effects(vec![effect])
            } else {
                let (effect, call_id) = step::ui_signature_effect(index, attempt);
                set_pending(ctx, call_id);
                ctx.set_fact(LOCATING_PHASE_FACT, "baseline");
                Reaction::effects(vec![effect])
            }
        }

        // Resolve the tap target in the hierarchy. Not found is a real,
        // reportable outcome here - see `step::FoundElement`.
        "locate" => match step::parse_find_element(observation) {
            Ok(found) if found.found => {
                ctx.set_fact(BASELINE_SIG_FACT, json!(found.signature));
                let (effect, call_id) = step::tap_pixel_effect(index, attempt, found.x, found.y);
                set_pending(ctx, call_id);
                Reaction::transition(
                    UiTestState::Acting,
                    vec![effect],
                    "element located in the view hierarchy; tapping",
                )
            }
            Ok(found) => {
                let target = load_compiled(ctx)
                    .and_then(|c| c.target)
                    .unwrap_or_default();
                let seen = if found.candidates.is_empty() {
                    "nothing tappable was on screen".to_string()
                } else {
                    format!("on screen: {}", found.candidates.join(", "))
                };
                fail_or_retry(ctx, format!("'{target}' is not on this screen ({seen})"))
            }
            Err(e) => fail_or_retry(ctx, e),
        },

        // Pre-action baseline for a verb that needs no target resolution.
        "baseline" => match step::parse_signature(observation) {
            Ok(signature) => {
                ctx.set_fact(BASELINE_SIG_FACT, json!(signature));
                let Some(compiled) = load_compiled(ctx) else {
                    return fail_or_retry(ctx, "internal error: no compiled step to act on");
                };
                match step::direct_action_effect(ctx, &compiled, index, attempt) {
                    Ok((effect, call_id)) => {
                        set_pending(ctx, call_id);
                        ctx.set_fact(ASK_BIND_FACT, Value::Null);
                        Reaction::transition(
                            UiTestState::Acting,
                            vec![effect],
                            "baseline recorded; performing the action",
                        )
                    }
                    Err(reason) => fail_or_retry(ctx, reason),
                }
            }
            Err(e) => fail_or_retry(ctx, e),
        },

        other => fail_or_retry(
            ctx,
            format!("internal error: unexpected locating phase '{other}'"),
        ),
    }
}
