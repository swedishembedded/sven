// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Read-only questions about a device's CURRENT state: where an element is,
//! whether anything changed, and whether the screen can be driven at all.
//!
//! Split from `tool.rs`, which holds the verbs that DRIVE the device. These
//! answer rather than act, and they are what let a UI test fail honestly:
//! `find_element` can report that a target is not on screen, which is the
//! one answer a generative grounding model structurally cannot give.
//!
//! Swedish Embedded AB implements solutions for deterministic Android UI
//! automation for its clients. If your team needs expertise in on-device
//! test automation that does not guess, you can procure our services by
//! sending an email to info@swedishembedded.com.

use serde_json::{json, Value};

use sven_tool_api::tool::ToolOutput;

use crate::adb;
use crate::ui_tree;

/// Dump the device's current view hierarchy as XML.
///
/// `uiautomator dump` writes to a file on the device, so this is two round
/// trips: produce it, then stream it back with `exec-out cat` (which,
/// unlike `shell`, does not mangle binary or rewrite line endings).
pub(crate) async fn dump_ui_hierarchy(serial: &str) -> Result<String, String> {
    const REMOTE: &str = "/sdcard/sven-ui-dump.xml";

    let dumped = adb::run(serial, &["shell", "uiautomator", "dump", REMOTE], 30).await?;
    if !dumped.success() {
        return Err(format!("uiautomator dump failed: {}", dumped.stderr));
    }

    let read = adb::run(serial, &["exec-out", "cat", REMOTE], 30).await?;
    if read.stdout.is_empty() {
        return Err(format!(
            "view hierarchy came back empty (stderr: {})",
            read.stderr
        ));
    }
    Ok(String::from_utf8_lossy(&read.stdout).into_owned())
}

/// Shape the `find_element` answer from an already-dumped hierarchy.
///
/// Split out from the device round trip so the part with all the judgement
/// in it is testable without a phone.
fn find_element_answer(xml: &str, target: &str) -> Value {
    let elements = ui_tree::parse(xml);
    let signature = ui_tree::signature(&elements);

    match ui_tree::find(&elements, target) {
        Some(m) => {
            let el = &elements[m.index];
            let (x, y) = el.bounds.center();
            let via = match m.kind {
                ui_tree::MatchKind::Exact => "exact",
                ui_tree::MatchKind::Contains => "contains",
                ui_tree::MatchKind::Fuzzy => "fuzzy",
            };
            json!({
                "found": true,
                "x": x,
                "y": y,
                "label": m.label,
                "score": m.score,
                "via": via,
                "class": el.class,
                "bounds": [el.bounds.x0, el.bounds.y0, el.bounds.x1, el.bounds.y1],
                "signature": signature,
            })
        }
        // No coordinate is offered on a miss, deliberately. Handing back a
        // best-effort guess is exactly how a tap lands in empty space and
        // the step still reports success.
        None => json!({
            "found": false,
            "candidates": ui_tree::candidates(&elements),
            "signature": signature,
        }),
    }
}

pub(crate) async fn find_element(call_id: &str, serial: &str, args: &Value) -> ToolOutput {
    let target = match args.get("target").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t,
        _ => return ToolOutput::err(call_id, "find_element requires a non-empty 'target'"),
    };
    let xml = match dump_ui_hierarchy(serial).await {
        Ok(x) => x,
        Err(e) => return ToolOutput::err(call_id, e),
    };
    ToolOutput::ok(call_id, find_element_answer(&xml, target).to_string())
}

pub(crate) async fn ui_signature(call_id: &str, serial: &str) -> ToolOutput {
    let xml = match dump_ui_hierarchy(serial).await {
        Ok(x) => x,
        Err(e) => return ToolOutput::err(call_id, e),
    };
    let signature = ui_tree::signature(&ui_tree::parse(&xml));
    ToolOutput::ok(call_id, json!({ "signature": signature }).to_string())
}

/// Whether `dumpsys power` reports the display as not awake.
///
/// Android's `mWakefulness` is one of `Awake`, `Asleep`, `Dozing`,
/// `Dreaming`. Only `Awake` means a caller can see or drive the screen.
///
/// This exists because a powered-off display screencaps as a solid black
/// frame - which is EXACTLY what a FLAG_SECURE frame looks like. Without
/// this check a sleeping phone is diagnosed as "a screen a human must
/// handle", which is both wrong and unactionable; the real answer is "wake
/// the device". Measured on a real device mid-run: the screen slept, and
/// the secure-screen gate would have reported a secure screen.
fn display_is_off(dumpsys_power: &str) -> bool {
    dumpsys_power
        .lines()
        .find_map(|l| l.trim().strip_prefix("mWakefulness="))
        .is_some_and(|state| !state.trim().eq_ignore_ascii_case("Awake"))
}

/// Whether the current frame is Android's solid-black `FLAG_SECURE`
/// placeholder.
///
/// Separate from `screenshot` so a caller can gate on it without also
/// committing to writing a PNG somewhere, and so the answer is a typed
/// boolean rather than something parsed back out of prose.
pub(crate) async fn screen_is_secure(call_id: &str, serial: &str) -> ToolOutput {
    // Ask whether the display is even on BEFORE reading pixels: an off
    // display is black, and a black frame is the FLAG_SECURE signal.
    match adb::run(serial, &["shell", "dumpsys", "power"], 20).await {
        Ok(power) if display_is_off(&String::from_utf8_lossy(&power.stdout)) => {
            return ToolOutput::ok(
                call_id,
                json!({ "secure_screen": false, "display_off": true }).to_string(),
            );
        }
        Ok(_) => {}
        // A device that will not answer `dumpsys` is a bigger problem than
        // this check; fall through and let the capture itself report it.
        Err(_) => {}
    }

    let out = match adb::run(serial, &["exec-out", "screencap", "-p"], 20).await {
        Ok(o) => o,
        Err(e) => return ToolOutput::err(call_id, e),
    };
    if out.stdout.is_empty() {
        return ToolOutput::err(
            call_id,
            format!("screencap produced no output (stderr: {})", out.stderr),
        );
    }
    let img = match image::load_from_memory(&out.stdout) {
        Ok(i) => i,
        Err(e) => return ToolOutput::err(call_id, format!("could not decode screencap: {e}")),
    };
    ToolOutput::ok(
        call_id,
        json!({
            "secure_screen": sven_image::is_flag_secure_black(&img),
            "display_off": false,
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: &str = r#"<hierarchy>
      <node class="android.widget.Button" text="" content-desc="Sign in with Mobile BankID" clickable="true" enabled="true" bounds="[185,1777][1036,1939]">
        <node class="android.widget.TextView" text="Sign in with Mobile BankID" content-desc="" clickable="false" enabled="true" bounds="[377,1825][994,1891]" />
      </node>
      <node class="android.widget.TextView" text="Pay bills" content-desc="" clickable="false" enabled="true" bounds="[45,1075][1174,1321]" />
    </hierarchy>"#;

    /// A found target answers with the pixel centre of the CLICKABLE node,
    /// which is what a tap has to use - not the label's own box.
    #[test]
    fn find_element_answers_with_the_clickable_centre() {
        let v = find_element_answer(SCREEN, "Sign in with Mobile BankID");
        assert_eq!(v["found"], json!(true));
        assert_eq!(v["x"], json!(610));
        assert_eq!(v["y"], json!(1858));
        assert_eq!(v["via"], json!("exact"));
        assert!(v["signature"].as_str().is_some_and(|s| !s.is_empty()));
    }

    /// The whole reason this exists: a phrase that is not on screen must
    /// come back not-found, carrying what WAS there so the failure is
    /// actionable rather than just red.
    #[test]
    fn find_element_reports_not_found_with_the_real_candidates() {
        let v = find_element_answer(SCREEN, "Logga in");
        assert_eq!(v["found"], json!(false));
        assert!(v.get("x").is_none(), "no coordinate may be offered");
        let candidates = v["candidates"].as_array().expect("candidates listed");
        assert!(candidates.contains(&json!("Sign in with Mobile BankID")));
    }

    /// Both answers carry a signature, so the caller always has the
    /// pre-action baseline it needs to verify the action did something.
    #[test]
    fn a_signature_is_reported_whether_or_not_the_target_was_found() {
        let hit = find_element_answer(SCREEN, "Sign in with Mobile BankID");
        let miss = find_element_answer(SCREEN, "Logga in");
        assert_eq!(hit["signature"], miss["signature"]);
    }

    /// A sleeping phone screencaps solid black, which is the FLAG_SECURE
    /// signal - so wakefulness has to be consulted first or a dozing device
    /// is reported as a screen only a human may touch.
    #[test]
    fn a_display_that_is_not_awake_is_recognised_as_off() {
        for state in ["Asleep", "Dozing", "Dreaming"] {
            assert!(
                display_is_off(&format!("  mWakefulness={state}\n  mDirty=0")),
                "{state} must count as off"
            );
        }
        assert!(!display_is_off("  mWakefulness=Awake\n  mDirty=0"));
    }

    /// Absent the field entirely, assume the display is usable rather than
    /// blocking a run on a parse miss.
    #[test]
    fn an_unreadable_power_dump_does_not_claim_the_display_is_off() {
        assert!(!display_is_off("something else entirely"));
    }
}
