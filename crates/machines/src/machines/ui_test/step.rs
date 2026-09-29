// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The step compiler: turns one natural-language instruction line (free-form
//! prose in whatever language the test script is written in - nothing here
//! assumes English or any other particular language; e.g. "Launch the demo
//! app", "Click \"log in with password\"", "Enter the code") into a
//! [`CompiledStep`] via one bounded, schema-constrained
//! text-model call (instruction-following only, never vision - a tap's
//! target is resolved in the device's own view hierarchy, not by a model).
//! Also builds every
//! `Effect::CallTool` [`super::UiTestMachine`] emits and parses their
//! results, so the machine itself only ever handles typed values.
//!
//! Tool calls are named by string, not by type, for the same reason
//! `reactive_agent.rs`'s `ASK_QUESTION_TOOL` is: `sven-machines` is a
//! machines-tier crate and (by deliberate architectural discipline, not tier
//! legality - domain is a strictly lower tier and could be depended on) has
//! no path to `sven-tools-android`'s implementation.
//! The kernel still enforces the declared capability; this only names which
//! bucket a call falls in.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sven_hsm::{context::Context, effect::Effect, ids::ToolCallId, permissions::ToolCapability};
use sven_vocab::TurnRequest;

use super::vars;

pub const ANDROID_TOOL: &str = "android";
pub const ANDROID_CAPABILITY: ToolCapability = ToolCapability::ControlDevice;
pub const ASK_QUESTION_TOOL: &str = "ask_question";
/// Mirrors `AskQuestionTool::kernel_capability()` (see `reactive_agent.rs`'s
/// identical constant and its reasoning).
pub const ASK_QUESTION_CAPABILITY: ToolCapability = ToolCapability::ReadFile;

/// Conversation thread every step-compiler call appends to.
pub const COMPILE_THREAD: &str = "ui_test_compile";

/// The action verbs a compiled step may name - exactly the subset of
/// `sven-tools-android`'s verb set (plus `ask_user`) this machine drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepVerb {
    /// Bring an app to the foreground. `target` is a package-name hint (no
    /// fuzzy app-name resolution yet - see the roadmap's Phase 3 gaps).
    LaunchApp,
    /// Force-stop an app. `target` is a package-name hint.
    ForceStop,
    /// Tap a described on-screen element. `target` is the phrase/element
    /// text a screenshot is grounded against.
    Tap,
    /// Type into the currently focused field. `value` or `value_ref`.
    TypeText,
    /// Swipe in a fixed direction. `target` is "up"/"down"/"left"/"right".
    Swipe,
    /// Send a key event. `target` (or `value`) is the key name.
    KeyEvent,
    /// Sleep. `value` is milliseconds as a string (default 500 if absent).
    Wait,
    /// Ask the human. `target` is the question; `bind` optionally names the
    /// variable a later step's `value_ref` can reference.
    AskUser,
}

/// One compiled instruction. Exactly what a bounded, schema-constrained LLM
/// call produces from one natural-language step line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledStep {
    pub verb: StepVerb,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub value_ref: Option<String>,
    #[serde(default)]
    pub bind: Option<String>,
}

/// The JSON Schema a step-compiler turn's response is constrained to.
pub fn step_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "verb": {
                "type": "string",
                "enum": ["launch_app", "force_stop", "tap", "type_text", "swipe", "key_event", "wait", "ask_user"]
            },
            "target": { "type": ["string", "null"] },
            "value": { "type": ["string", "null"] },
            "value_ref": { "type": ["string", "null"] },
            "bind": { "type": ["string", "null"] }
        },
        "required": ["verb"],
        "additionalProperties": false
    })
}

/// Build the bounded, schema-constrained `Effect::CallLlm` that compiles one
/// natural-language step line. Instruction-following only - no tools, no
/// image - so `TurnExecutor` cannot offer the model anything to call and the
/// turn always resolves in one round.
pub fn compile_step_effect(step_text: &str, known_vars: &[String]) -> Effect {
    let vars_note = if known_vars.is_empty() {
        "(none yet)".to_string()
    } else {
        known_vars.join(", ")
    };
    let instruction = format!(
        "Compile one UI-test step into a single structured action. The step \
         may be written in English or Swedish. Respond with exactly one JSON \
         object matching the given schema - no prose, no markdown fences.\n\n\
         Verbs: launch_app/force_stop (target = app or package name hint), \
         tap (target = the visible text/element to find on screen), \
         type_text (value = literal text, or value_ref = a previously bound \
         variable name), swipe (target = up/down/left/right), key_event \
         (target = key name, e.g. back/enter/home), wait (value = \
         milliseconds), ask_user (target = the question to show a human; set \
         bind to a short lowercase slug naming the variable a later step can \
         reference via value_ref, e.g. \"code\").\n\n\
         Variables already bound in this run: {vars_note}\n\n\
         Step: \"{step_text}\""
    );
    Effect::CallLlm {
        request: TurnRequest {
            thread: COMPILE_THREAD.to_string(),
            instruction,
            tools: vec![],
            all_tools_mode: String::new(),
            schema: step_schema(),
            schema_name: "ui_test_step".to_string(),
            model: None,
            dynamic_suffix: None,
            max_tool_rounds: Some(1),
            refused_calls: Vec::new(),
        }
        .to_value(),
    }
}

/// Parse a step-compiler turn's response text into a [`CompiledStep`].
///
/// Tolerates a model wrapping its JSON in a markdown code fence, the one
/// deviation real models are prone to even under a strict schema.
pub fn parse_compiled_step(text: &str) -> Result<CompiledStep, String> {
    let cleaned = strip_code_fence(text.trim());
    serde_json::from_str::<CompiledStep>(cleaned).map_err(|e| {
        format!(
            "could not parse compiled step JSON: {e} (text: {:.256})",
            cleaned
        )
    })
}

fn strip_code_fence(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("```") else {
        return s;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest).trim_start();
    match rest.rfind("```") {
        Some(end) => rest[..end].trim(),
        None => rest.trim(),
    }
}

/// The `android` tool's `find_element` answer.
///
/// `found: false` carries no coordinate at all - that is the point. A
/// grounding model always returns some box, so a step could never fail for
/// pointing at something that was not on screen; `candidates` names what
/// WAS there so the failure is actionable.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FoundElement {
    pub found: bool,
    #[serde(default)]
    pub x: i64,
    #[serde(default)]
    pub y: i64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub via: String,
    #[serde(default)]
    pub candidates: Vec<String>,
    /// Digest of the hierarchy this answer was read from - the pre-action
    /// baseline the post-action check compares against.
    #[serde(default)]
    pub signature: String,
}

/// Parse a `find_element` observation.
pub fn parse_find_element(observation: &Value) -> Result<FoundElement, String> {
    let text = observation
        .as_str()
        .ok_or_else(|| "find_element observation was not a string".to_string())?;
    serde_json::from_str(text).map_err(|e| format!("could not parse find_element result: {e}"))
}

/// Parse a `ui_signature` observation into the digest itself.
pub fn parse_signature(observation: &Value) -> Result<String, String> {
    let text = observation
        .as_str()
        .ok_or_else(|| "ui_signature observation was not a string".to_string())?;
    let v: Value = serde_json::from_str(text)
        .map_err(|e| format!("could not parse ui_signature result: {e}"))?;
    v.get("signature")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "ui_signature result carried no 'signature'".to_string())
}

/// The `screen_is_secure` answer: whether a human must take over, and
/// whether the display is simply not on.
///
/// These are different problems with different remedies and must not be
/// conflated. A powered-off display screencaps solid black, which is also
/// the FLAG_SECURE signal - so without `display_off` a sleeping phone is
/// diagnosed as "a screen automation must never drive", which is wrong and
/// leaves the operator nothing to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ScreenState {
    pub secure_screen: bool,
    #[serde(default)]
    pub display_off: bool,
}

/// Parse a `screen_is_secure` observation.
pub fn parse_secure_screen(observation: &Value) -> Result<ScreenState, String> {
    let text = observation
        .as_str()
        .ok_or_else(|| "screen_is_secure observation was not a string".to_string())?;
    serde_json::from_str(text).map_err(|e| format!("could not parse screen_is_secure result: {e}"))
}

// ─── Effect builders ────────────────────────────────────────────────────────

pub fn secure_check_effect(index: u32, attempt: u32) -> (Effect, ToolCallId) {
    let call_id = derive_call_id(&format!("ui_test:secure:{index}:{attempt}"));
    (
        Effect::CallTool {
            call_id,
            name: ANDROID_TOOL.to_string(),
            capability: ANDROID_CAPABILITY,
            args: json!({ "action": "screen_is_secure" }),
        },
        call_id,
    )
}

pub fn find_element_effect(index: u32, attempt: u32, target: &str) -> (Effect, ToolCallId) {
    let call_id = derive_call_id(&format!("ui_test:locate:{index}:{attempt}"));
    (
        Effect::CallTool {
            call_id,
            name: ANDROID_TOOL.to_string(),
            capability: ANDROID_CAPABILITY,
            args: json!({ "action": "find_element", "target": target }),
        },
        call_id,
    )
}

pub fn ui_signature_effect(index: u32, attempt: u32) -> (Effect, ToolCallId) {
    let call_id = derive_call_id(&format!("ui_test:verify:{index}:{attempt}"));
    (
        Effect::CallTool {
            call_id,
            name: ANDROID_TOOL.to_string(),
            capability: ANDROID_CAPABILITY,
            args: json!({ "action": "ui_signature" }),
        },
        call_id,
    )
}

/// Tap an exact pixel coordinate, as `find_element` reports it.
///
/// Pixels, not normalized fractions: the view hierarchy states real bounds,
/// so converting to a fraction and back would only add rounding.
pub fn tap_pixel_effect(index: u32, attempt: u32, x: i64, y: i64) -> (Effect, ToolCallId) {
    let call_id = derive_call_id(&format!("ui_test:act:{index}:{attempt}"));
    (
        Effect::CallTool {
            call_id,
            name: ANDROID_TOOL.to_string(),
            capability: ANDROID_CAPABILITY,
            args: json!({ "action": "tap", "x": x, "y": y, "normalized": false }),
        },
        call_id,
    )
}

pub fn tap_effect(index: u32, attempt: u32, x: f64, y: f64) -> (Effect, ToolCallId) {
    let call_id = derive_call_id(&format!("ui_test:act:{index}:{attempt}"));
    (
        Effect::CallTool {
            call_id,
            name: ANDROID_TOOL.to_string(),
            capability: ANDROID_CAPABILITY,
            args: json!({ "action": "tap", "x": x, "y": y, "normalized": true }),
        },
        call_id,
    )
}

pub fn ask_user_effect(
    index: u32,
    attempt: u32,
    question: &str,
    options: Vec<String>,
) -> (Effect, ToolCallId) {
    let call_id = derive_call_id(&format!("ui_test:act:{index}:{attempt}"));
    (
        Effect::CallTool {
            call_id,
            name: ASK_QUESTION_TOOL.to_string(),
            capability: ASK_QUESTION_CAPABILITY,
            args: json!({
                "questions": [{
                    "prompt": question,
                    "options": options,
                    "allow_multiple": false,
                }]
            }),
        },
        call_id,
    )
}

/// Build the direct `android` tool call for every verb that needs neither
/// grounding nor a human (everything except [`StepVerb::Tap`] and
/// [`StepVerb::AskUser`], which the machine handles in their own phases).
pub fn direct_action_effect(
    ctx: &Context,
    compiled: &CompiledStep,
    index: u32,
    attempt: u32,
) -> Result<(Effect, ToolCallId), String> {
    let call_id = derive_call_id(&format!("ui_test:act:{index}:{attempt}"));
    let args = match compiled.verb {
        StepVerb::LaunchApp => {
            let target = compiled
                .target
                .clone()
                .ok_or("launch_app step compiled with no target")?;
            json!({ "action": "launch_app", "package": target })
        }
        StepVerb::ForceStop => {
            let target = compiled
                .target
                .clone()
                .ok_or("force_stop step compiled with no target")?;
            json!({ "action": "force_stop", "package": target })
        }
        StepVerb::TypeText => {
            let text = resolve_value(ctx, compiled)?;
            json!({ "action": "type_text", "text": text })
        }
        StepVerb::Swipe => {
            let dir = compiled.target.clone().unwrap_or_else(|| "up".to_string());
            let (x, y, x2, y2) = swipe_coords(&dir)?;
            json!({ "action": "swipe", "x": x, "y": y, "x2": x2, "y2": y2, "normalized": true })
        }
        StepVerb::KeyEvent => {
            let key = compiled
                .target
                .clone()
                .or_else(|| compiled.value.clone())
                .ok_or("key_event step compiled with no key")?;
            json!({ "action": "key_event", "key": key })
        }
        StepVerb::Wait => {
            let ms: u64 = compiled
                .value
                .as_deref()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500);
            json!({ "action": "wait", "ms": ms })
        }
        StepVerb::Tap | StepVerb::AskUser => {
            return Err(
                "direct_action_effect called with a verb that needs its own phase".to_string(),
            )
        }
    };
    Ok((
        Effect::CallTool {
            call_id,
            name: ANDROID_TOOL.to_string(),
            capability: ANDROID_CAPABILITY,
            args,
        },
        call_id,
    ))
}

fn resolve_value(ctx: &Context, compiled: &CompiledStep) -> Result<String, String> {
    if let Some(v) = &compiled.value {
        return Ok(v.clone());
    }
    if let Some(r) = &compiled.value_ref {
        return vars::resolve(ctx, r)
            .ok_or_else(|| format!("no bound value for '{r}' (was it ever asked?)"));
    }
    Err("type_text step compiled with neither 'value' nor 'value_ref'".to_string())
}

fn swipe_coords(dir: &str) -> Result<(f64, f64, f64, f64), String> {
    match dir.trim().to_lowercase().as_str() {
        "up" => Ok((0.5, 0.8, 0.5, 0.2)),
        "down" => Ok((0.5, 0.2, 0.5, 0.8)),
        "left" => Ok((0.8, 0.5, 0.2, 0.5)),
        "right" => Ok((0.2, 0.5, 0.8, 0.5)),
        other => Err(format!(
            "unsupported swipe direction '{other}' (expected up/down/left/right)"
        )),
    }
}

/// Strip a leading "Other:"/"other:" prefix `ask_question`'s free-form path
/// adds, and trim - so a step that binds a human's typed answer (e.g. a
/// one-time confirmation code) gets the value itself, not the tool's own
/// formatting.
#[must_use]
pub fn normalize_answer(text: &str) -> String {
    let t = text.trim();
    for prefix in ["Other: ", "Other:", "other: ", "other:"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    t.to_string()
}

/// Extract the answer half of `ask_question`'s plain-terminal formatting
/// (`"Q: <prompt>\nA: <answer>"`), if present; otherwise the text is already
/// the bare answer (the TUI and headless/parked paths both return it plain).
#[must_use]
pub fn extract_answer_text(content: &str) -> String {
    if let Some(idx) = content.find("\nA: ") {
        return normalize_answer(&content[idx + 4..]);
    }
    normalize_answer(content)
}

/// Deterministic [`ToolCallId`] derivation, so a replay of the same event
/// stream reproduces the same id `ToolSucceeded`/`ToolFailed` correlate
/// against. Mirrors `reactive_agent.rs`'s `derive_call_id` (duplicated
/// rather than shared: it is a handful of lines with no state, and the two
/// machines' id spaces must never collide by construction, which is
/// automatic since their label prefixes differ).
fn derive_call_id(label: &str) -> ToolCallId {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
        bytes
            .iter()
            .fold(seed, |h, b| (h ^ u64::from(*b)).wrapping_mul(FNV_PRIME))
    }

    let hi = fnv1a(FNV_OFFSET, label.as_bytes());
    let lo = fnv1a(FNV_OFFSET ^ FNV_PRIME, label.as_bytes());
    ToolCallId::from_uuid(uuid::Uuid::from_u128(
        (u128::from(hi) << 64) | u128::from(lo),
    ))
}

/// Known variable names, for the compiler prompt (see [`compile_step_effect`]).
#[must_use]
pub fn known_var_names(vars: &BTreeMap<String, String>) -> Vec<String> {
    vars.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_step_round_trips_through_the_schema_shape() {
        let step = CompiledStep {
            verb: StepVerb::Tap,
            target: Some("log in with password".to_string()),
            value: None,
            value_ref: None,
            bind: None,
        };
        let text = serde_json::to_string(&step).unwrap();
        assert_eq!(parse_compiled_step(&text).unwrap(), step);
    }

    #[test]
    fn parse_tolerates_a_markdown_code_fence() {
        let text = "```json\n{\"verb\": \"wait\", \"value\": \"500\"}\n```";
        let step = parse_compiled_step(text).unwrap();
        assert_eq!(step.verb, StepVerb::Wait);
        assert_eq!(step.value, Some("500".to_string()));
    }

    #[test]
    fn an_unknown_verb_is_a_clean_parse_error() {
        let err = parse_compiled_step(r#"{"verb": "explode"}"#).unwrap_err();
        assert!(err.contains("could not parse"), "{err}");
    }

    #[test]
    fn ask_user_step_carries_its_bind_name() {
        let step =
            parse_compiled_step(r#"{"verb": "ask_user", "target": "the code?", "bind": "code"}"#)
                .unwrap();
        assert_eq!(step.verb, StepVerb::AskUser);
        assert_eq!(step.bind, Some("code".to_string()));
    }

    #[test]
    fn type_text_step_carries_a_value_ref() {
        let step = parse_compiled_step(r#"{"verb": "type_text", "value_ref": "code"}"#).unwrap();
        assert_eq!(step.value_ref, Some("code".to_string()));
        assert_eq!(step.value, None);
    }

    #[test]
    fn direct_action_effect_resolves_a_value_ref_against_bound_vars() {
        let mut ctx = Context::new();
        vars::bind(&mut ctx, "code", "123456");
        let step = CompiledStep {
            verb: StepVerb::TypeText,
            target: None,
            value: None,
            value_ref: Some("code".into()),
            bind: None,
        };
        let (effect, _) = direct_action_effect(&ctx, &step, 0, 0).unwrap();
        let Effect::CallTool { args, .. } = effect else {
            panic!("expected CallTool")
        };
        assert_eq!(args["text"], "123456");
    }

    #[test]
    fn direct_action_effect_fails_cleanly_when_a_value_ref_is_unbound() {
        let ctx = Context::new();
        let step = CompiledStep {
            verb: StepVerb::TypeText,
            target: None,
            value: None,
            value_ref: Some("code".into()),
            bind: None,
        };
        let err = direct_action_effect(&ctx, &step, 0, 0).unwrap_err();
        assert!(err.contains("code"), "{err}");
    }

    #[test]
    fn direct_action_effect_rejects_tap_and_ask_user() {
        let ctx = Context::new();
        let step = CompiledStep {
            verb: StepVerb::Tap,
            target: Some("x".into()),
            value: None,
            value_ref: None,
            bind: None,
        };
        assert!(direct_action_effect(&ctx, &step, 0, 0).is_err());
    }

    #[test]
    fn swipe_direction_maps_to_fixed_normalized_coordinates() {
        let ctx = Context::new();
        let step = CompiledStep {
            verb: StepVerb::Swipe,
            target: Some("up".into()),
            value: None,
            value_ref: None,
            bind: None,
        };
        let (effect, _) = direct_action_effect(&ctx, &step, 0, 0).unwrap();
        let Effect::CallTool { args, .. } = effect else {
            panic!("expected CallTool")
        };
        assert_eq!(args["y"], 0.8);
        assert_eq!(args["y2"], 0.2);
    }

    #[test]
    fn an_unsupported_swipe_direction_is_a_clean_error() {
        let ctx = Context::new();
        let step = CompiledStep {
            verb: StepVerb::Swipe,
            target: Some("diagonal".into()),
            value: None,
            value_ref: None,
            bind: None,
        };
        let err = direct_action_effect(&ctx, &step, 0, 0).unwrap_err();
        assert!(err.contains("diagonal"), "{err}");
    }

    #[test]
    fn wait_defaults_to_500ms_when_no_value_given() {
        let ctx = Context::new();
        let step = CompiledStep {
            verb: StepVerb::Wait,
            target: None,
            value: None,
            value_ref: None,
            bind: None,
        };
        let (effect, _) = direct_action_effect(&ctx, &step, 0, 0).unwrap();
        let Effect::CallTool { args, .. } = effect else {
            panic!("expected CallTool")
        };
        assert_eq!(args["ms"], 500);
    }

    #[test]
    fn derive_call_id_is_stable_for_the_same_label() {
        let (_, id1) = secure_check_effect(2, 0);
        let (_, id2) = secure_check_effect(2, 0);
        assert_eq!(id1, id2);
    }

    #[test]
    fn derive_call_id_differs_across_phases_index_and_attempt() {
        let (_, secure_id) = secure_check_effect(0, 0);
        let (_, locate_id) = find_element_effect(0, 0, "x");
        let (_, tap_id) = tap_pixel_effect(0, 0, 10, 20);
        let (_, verify_id) = ui_signature_effect(0, 0);
        let (_, next_attempt_id) = secure_check_effect(0, 1);
        let (_, next_index_id) = secure_check_effect(1, 0);
        let ids = [
            secure_id,
            locate_id,
            tap_id,
            verify_id,
            next_attempt_id,
            next_index_id,
        ];
        for (i, a) in ids.iter().enumerate() {
            for (j, b) in ids.iter().enumerate() {
                assert_eq!(
                    i == j,
                    a == b,
                    "ids at {i} and {j} should only match themselves"
                );
            }
        }
    }

    #[test]
    fn extract_answer_text_handles_the_plain_terminal_qa_format() {
        assert_eq!(
            extract_answer_text("Q: What is the code?\nA: 123456"),
            "123456"
        );
    }

    #[test]
    fn extract_answer_text_strips_an_other_prefix() {
        assert_eq!(extract_answer_text("Other: 123456"), "123456");
        assert_eq!(extract_answer_text("123456"), "123456");
    }

    #[test]
    fn ask_user_effect_carries_the_offered_options() {
        let (effect, _) = ask_user_effect(
            0,
            0,
            "the code?",
            vec!["Provide value".into(), "Skip".into()],
        );
        let Effect::CallTool { args, name, .. } = effect else {
            panic!("expected CallTool")
        };
        assert_eq!(name, ASK_QUESTION_TOOL);
        assert_eq!(
            args["questions"][0]["options"],
            json!(["Provide value", "Skip"])
        );
    }
}
