// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`AndroidTool`] - a narrow, typed verb set for driving one Android device
//! over ADB: screenshot, tap, swipe, type text, send a key event, launch/stop
//! an app, list packages, read display/foreground-app info, and wait.
//!
//! Deliberately not a `shell` passthrough: a UI-test machine should be able
//! to control a physical device without also being granted arbitrary command
//! execution on the host (see `ToolCapability::ControlDevice`'s doc for why
//! that's a separate bucket from `ExecuteShell`). Coordinates a test script
//! (or a vision-grounding step upstream of this tool) supplies for `tap`/
//! `swipe` are normalized (`0.0..=1.0` fractions of the display) by default,
//! so a script never has to know a specific device's pixel resolution.

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput, ToolOutputPart};

use crate::adb;

/// Ceiling on a caller-supplied `ms` for the `wait` action - mirrors the
/// shell tool's timeout ceiling reasoning: unbounded would pin a UI-test run.
const MAX_WAIT_MS: u64 = 30_000;

pub struct AndroidTool {
    /// Device serial to target when a call doesn't supply one. `None` means
    /// "auto-detect the single attached ready device" (see
    /// [`adb::resolve_serial`]).
    default_serial: Option<String>,
    /// Packages the caller knows are installed on this device. Empty is the
    /// ordinary case; see [`AndroidTool::with_declared_packages`].
    declared_packages: Vec<String>,
}

impl AndroidTool {
    pub fn new(default_serial: Option<String>) -> Self {
        Self {
            default_serial,
            declared_packages: Vec::new(),
        }
    }

    /// Declares which packages this device is known to have installed, so a
    /// loose `launch_app`/`force_stop` hint resolves against that
    /// declaration before the device is asked.
    ///
    /// A caller that placed this run on a device it already has an app
    /// inventory for should pass it: resolution then costs no `pm list
    /// packages` round trip and cannot be derailed by an unrelated system
    /// package sharing a dot-segment with the hint. Leaving it empty (the
    /// default) simply asks the device, which is what every other caller
    /// wants. See [`adb::resolve_package_validated`] for the exact
    /// precedence and its limits.
    #[must_use]
    pub fn with_declared_packages(mut self, packages: Vec<String>) -> Self {
        self.declared_packages = packages;
        self
    }
}

impl Default for AndroidTool {
    /// `SVEN_ANDROID_SERIAL` lets a fleet/CI caller pin a device without
    /// threading a serial through every construction site; explicit
    /// [`AndroidTool::new`] still overrides it.
    fn default() -> Self {
        Self {
            default_serial: std::env::var("SVEN_ANDROID_SERIAL").ok(),
            declared_packages: Vec::new(),
        }
    }
}

#[async_trait]
impl Tool for AndroidTool {
    fn name(&self) -> &str {
        "android"
    }

    fn description(&self) -> &str {
        "Control one attached Android device over ADB. Required field: 'action'.\n\
         Actions:\n\
         - `display_info`   - display size in pixels.\n\
         - `screenshot`     - capture the current screen; returned as an image plus the saved path.\n\
         - `tap`            - tap at (x, y). Normalized 0.0-1.0 fractions of the display by default\n\
         (`normalized: false` for raw pixels).\n\
         - `swipe`          - swipe from (x, y) to (x2, y2); optional `duration_ms`.\n\
         - `type_text`      - type `text` into the currently focused field.\n\
         - `key_event`      - send `key` (e.g. \"BACK\", \"ENTER\", \"HOME\", \"DEL\", or a raw keycode).\n\
         - `go_home`        - press the HOME key.\n\
         - `launch_app`     - bring `package`'s launcher activity to the foreground. This RESUMES\n\
         the app's existing task if one is running (confirmed on a real device: it does not\n\
         reset navigation state) - call `force_stop` first when the caller needs a known,\n\
         reproducible starting screen (e.g. at the start of a UI-test flow).\n\
         - `force_stop`     - force-stop `package`. Combine with `launch_app` for a cold start.\n\
         - `list_packages`  - installed packages, optionally filtered by substring `filter`.\n\
         - `current_app`    - the foreground package/activity.\n\
         - `wait`           - sleep `ms` (capped at 30000) - use between an action and its next\n\
         screenshot to let an animation/transition settle.\n\
         Optional on every action: `serial` to pick a specific device when more than one is attached."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "display_info", "screenshot", "tap", "swipe", "type_text",
                        "key_event", "go_home", "launch_app", "force_stop",
                        "list_packages", "current_app", "wait"
                    ]
                },
                "serial": { "type": "string", "description": "Target device serial (optional if exactly one is attached)" },
                "path": { "type": "string", "description": "screenshot: output PNG path (optional; a temp path is generated otherwise)" },
                "x": { "type": "number" },
                "y": { "type": "number" },
                "x2": { "type": "number", "description": "swipe: end x" },
                "y2": { "type": "number", "description": "swipe: end y" },
                "normalized": { "type": "boolean", "description": "tap/swipe: x/y are 0.0-1.0 fractions of the display (default true)" },
                "duration_ms": { "type": "integer", "description": "swipe: gesture duration in milliseconds" },
                "text": { "type": "string", "description": "type_text: literal text to type" },
                "key": { "type": "string", "description": "key_event: key name or numeric keycode" },
                "package": { "type": "string", "description": "launch_app/force_stop: Android package name" },
                "filter": { "type": "string", "description": "list_packages: substring filter" },
                "ms": { "type": "integer", "description": "wait: milliseconds to sleep (capped at 30000)" }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ControlDevice
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolOutput::err(&call.id, "missing required parameter 'action'"),
        };

        let explicit_serial = call.args.get("serial").and_then(|v| v.as_str());
        let serial =
            match adb::resolve_serial(explicit_serial, self.default_serial.as_deref()).await {
                Ok(s) => s,
                Err(e) => return ToolOutput::err(&call.id, e),
            };

        debug!(action, serial, "android tool");

        match action {
            "display_info" => display_info(&call.id, &serial).await,
            "screenshot" => {
                screenshot(
                    &call.id,
                    &serial,
                    call.args.get("path").and_then(|v| v.as_str()),
                )
                .await
            }
            "tap" => tap(&call.id, &serial, &call.args).await,
            "swipe" => swipe(&call.id, &serial, &call.args).await,
            "type_text" => type_text(&call.id, &serial, &call.args).await,
            "key_event" => key_event(&call.id, &serial, &call.args).await,
            "go_home" => send_key(&call.id, &serial, "KEYCODE_HOME").await,
            "launch_app" => {
                launch_app(&call.id, &serial, &call.args, &self.declared_packages).await
            }
            "force_stop" => {
                force_stop(&call.id, &serial, &call.args, &self.declared_packages).await
            }
            "list_packages" => {
                list_packages(
                    &call.id,
                    &serial,
                    call.args.get("filter").and_then(|v| v.as_str()),
                )
                .await
            }
            "current_app" => current_app(&call.id, &serial).await,
            "wait" => wait(&call.id, &call.args).await,
            other => ToolOutput::err(&call.id, format!("unknown action '{other}'")),
        }
    }
}

impl ToolDisplay for AndroidTool {
    fn display_name(&self) -> &str {
        "Android"
    }
    fn category(&self) -> &str {
        "system"
    }
    fn collapsed_summary(&self, args: &serde_json::Value) -> String {
        args.get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }
}

// ─── Action implementations ─────────────────────────────────────────────────

async fn display_size(serial: &str) -> Result<(u32, u32), String> {
    let out = adb::run(serial, &["shell", "wm", "size"], adb::DEFAULT_TIMEOUT_SECS).await?;
    adb::parse_wm_size(&out.stdout_text())
        .ok_or_else(|| format!("could not parse 'wm size' output: {:?}", out.stdout_text()))
}

async fn display_info(call_id: &str, serial: &str) -> ToolOutput {
    match display_size(serial).await {
        Ok((w, h)) => ToolOutput::ok(call_id, format!("{w}x{h}")),
        Err(e) => ToolOutput::err(call_id, e),
    }
}

async fn screenshot(call_id: &str, serial: &str, path: Option<&str>) -> ToolOutput {
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

    let owned_path;
    let path: &std::path::Path = match path {
        Some(p) => std::path::Path::new(p),
        None => {
            let millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            owned_path = std::env::temp_dir().join(format!("android-{serial}-{millis}.png"));
            owned_path.as_path()
        }
    };

    if let Err(e) = std::fs::write(path, &out.stdout) {
        return ToolOutput::err(
            call_id,
            format!("failed to write screenshot to {path:?}: {e}"),
        );
    }

    // Real device-pixel dimensions, read from the captured bytes directly -
    // `sven_image::load_image` may downscale for the model-facing data URL
    // (observed live: a 1220x2712 screenshot encoded as 921x2047), and the
    // label here must reflect the actual screen a `tap`'s pixel math targets,
    // not what the vision model happens to be shown.
    let real_dims = image::load_from_memory(&out.stdout)
        .ok()
        .map(|i| (i.width(), i.height()));

    match sven_image::load_image(path) {
        Ok(img) => {
            let data_url = img.into_data_url();
            let dims_text = real_dims
                .map(|(w, h)| format!(" ({w}x{h})"))
                .unwrap_or_default();
            ToolOutput::with_parts(
                call_id,
                vec![
                    ToolOutputPart::Text(format!(
                        "screenshot saved: {}{}",
                        path.display(),
                        dims_text
                    )),
                    ToolOutputPart::Image(data_url),
                ],
            )
        }
        Err(e) => ToolOutput::err(
            call_id,
            format!("captured but failed to load {path:?}: {e}"),
        ),
    }
}

fn want_normalized(args: &Value) -> bool {
    args.get("normalized")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

fn require_f64(args: &Value, field: &str) -> Result<f64, String> {
    args.get(field)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| format!("missing or non-numeric required parameter '{field}'"))
}

async fn to_pixels(serial: &str, x: f64, y: f64, normalized: bool) -> Result<(i64, i64), String> {
    if !normalized {
        return Ok((x.round() as i64, y.round() as i64));
    }
    if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err(format!(
            "normalized coordinates must be within 0.0..=1.0, got ({x}, {y})"
        ));
    }
    let (w, h) = display_size(serial).await?;
    Ok(((x * w as f64).round() as i64, (y * h as f64).round() as i64))
}

async fn tap(call_id: &str, serial: &str, args: &Value) -> ToolOutput {
    let (x, y) = match (require_f64(args, "x"), require_f64(args, "y")) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) | (_, Err(e)) => return ToolOutput::err(call_id, e),
    };
    let (px, py) = match to_pixels(serial, x, y, want_normalized(args)).await {
        Ok(p) => p,
        Err(e) => return ToolOutput::err(call_id, e),
    };
    let px_s = px.to_string();
    let py_s = py.to_string();
    match adb::run(
        serial,
        &["shell", "input", "tap", &px_s, &py_s],
        adb::DEFAULT_TIMEOUT_SECS,
    )
    .await
    {
        Ok(out) if out.success() => ToolOutput::ok(call_id, format!("tapped ({px}, {py})")),
        Ok(out) => ToolOutput::err(call_id, format!("tap failed: {}", out.stderr)),
        Err(e) => ToolOutput::err(call_id, e),
    }
}

async fn swipe(call_id: &str, serial: &str, args: &Value) -> ToolOutput {
    let (x, y, x2, y2) = match (
        require_f64(args, "x"),
        require_f64(args, "y"),
        require_f64(args, "x2"),
        require_f64(args, "y2"),
    ) {
        (Ok(x), Ok(y), Ok(x2), Ok(y2)) => (x, y, x2, y2),
        (a, b, c, d) => {
            let e = [a.err(), b.err(), c.err(), d.err()]
                .into_iter()
                .flatten()
                .next()
                .unwrap();
            return ToolOutput::err(call_id, e);
        }
    };
    let normalized = want_normalized(args);
    let (px, py) = match to_pixels(serial, x, y, normalized).await {
        Ok(p) => p,
        Err(e) => return ToolOutput::err(call_id, e),
    };
    let (px2, py2) = match to_pixels(serial, x2, y2, normalized).await {
        Ok(p) => p,
        Err(e) => return ToolOutput::err(call_id, e),
    };
    let duration = args
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(300);
    let (a, b, c, d, dur) = (
        px.to_string(),
        py.to_string(),
        px2.to_string(),
        py2.to_string(),
        duration.to_string(),
    );
    match adb::run(
        serial,
        &["shell", "input", "swipe", &a, &b, &c, &d, &dur],
        adb::DEFAULT_TIMEOUT_SECS,
    )
    .await
    {
        Ok(out) if out.success() => ToolOutput::ok(
            call_id,
            format!("swiped ({px}, {py}) -> ({px2}, {py2}) over {duration}ms"),
        ),
        Ok(out) => ToolOutput::err(call_id, format!("swipe failed: {}", out.stderr)),
        Err(e) => ToolOutput::err(call_id, e),
    }
}

async fn type_text(call_id: &str, serial: &str, args: &Value) -> ToolOutput {
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return ToolOutput::err(call_id, "missing required parameter 'text'"),
    };
    // `input text` only reliably handles ASCII (no on-device IME injection for
    // non-Latin scripts) - fail clearly rather than silently mangling input.
    if !text.is_ascii() {
        return ToolOutput::err(
            call_id,
            "'text' must be ASCII: Android's 'input text' does not reliably type non-ASCII \
             characters (no IME-level injection here yet)",
        );
    }
    let quoted = adb::shell_single_quote(text);
    let cmd = format!("input text {quoted}");
    match adb::run(serial, &["shell", &cmd], adb::DEFAULT_TIMEOUT_SECS).await {
        Ok(out) if out.success() => ToolOutput::ok(
            call_id,
            format!("typed {} characters", text.chars().count()),
        ),
        Ok(out) => ToolOutput::err(call_id, format!("type_text failed: {}", out.stderr)),
        Err(e) => ToolOutput::err(call_id, e),
    }
}

fn normalize_keycode(key: &str) -> String {
    if key.chars().all(|c| c.is_ascii_digit()) || key.starts_with("KEYCODE_") {
        key.to_string()
    } else {
        format!("KEYCODE_{}", key.to_uppercase())
    }
}

async fn send_key(call_id: &str, serial: &str, keycode: &str) -> ToolOutput {
    match adb::run(
        serial,
        &["shell", "input", "keyevent", keycode],
        adb::DEFAULT_TIMEOUT_SECS,
    )
    .await
    {
        Ok(out) if out.success() => ToolOutput::ok(call_id, format!("sent {keycode}")),
        Ok(out) => ToolOutput::err(call_id, format!("key_event failed: {}", out.stderr)),
        Err(e) => ToolOutput::err(call_id, e),
    }
}

async fn key_event(call_id: &str, serial: &str, args: &Value) -> ToolOutput {
    let key = match args.get("key").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return ToolOutput::err(call_id, "missing required parameter 'key'"),
    };
    send_key(call_id, serial, &normalize_keycode(key)).await
}

/// Resolves a caller-supplied app `hint` to a package actually installed on
/// `serial`, or a human-readable refusal naming what it saw.
///
/// `launch_app`/`force_stop` take a "package name hint" by contract (see
/// `ui_test`'s step-compiler prompt, which says so in as many words), but
/// `monkey -p`/`am force-stop` need a real package name. Without this, a
/// step written as "Launch betalo app" compiles to the hint `betalo` and
/// fails against a device that genuinely has `se.betalo.androidapp`
/// installed.
///
/// `declared` short-circuits the device round trip when the caller already
/// knows this device's app inventory - see
/// [`AndroidTool::with_declared_packages`].
async fn resolve_package_or_refuse(
    serial: &str,
    hint: &str,
    declared: &[String],
) -> Result<String, String> {
    match adb::resolve_package_validated(&adb::RealPackageLister, serial, hint, declared).await? {
        adb::PackagePick::Resolved(p) => Ok(p),
        adb::PackagePick::ResolvedFromHint(p) => {
            tracing::warn!(
                hint = %hint,
                resolved = %p,
                "app hint '{hint}' is not an installed package name; resolved it to '{p}'"
            );
            Ok(p)
        }
        adb::PackagePick::NoneInstalled => Err(format!(
            "no installed package matches '{hint}' (is it installed on this device?)"
        )),
        adb::PackagePick::Ambiguous(candidates) => Err(format!(
            "'{hint}' matches {} installed packages ({}); name the package exactly to disambiguate",
            candidates.len(),
            candidates.join(", ")
        )),
    }
}

async fn launch_app(call_id: &str, serial: &str, args: &Value, declared: &[String]) -> ToolOutput {
    let package = match args.get("package").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ToolOutput::err(call_id, "missing required parameter 'package'"),
    };
    if !adb::valid_package_name(package) {
        return ToolOutput::err(
            call_id,
            format!("'{package}' is not a valid Android package name"),
        );
    }
    let package = &match resolve_package_or_refuse(serial, package, declared).await {
        Ok(p) => p,
        Err(e) => return ToolOutput::err(call_id, e),
    };
    match adb::run(
        serial,
        &[
            "shell",
            "monkey",
            "-p",
            package,
            "-c",
            "android.intent.category.LAUNCHER",
            "1",
        ],
        adb::DEFAULT_TIMEOUT_SECS,
    )
    .await
    {
        Ok(out) if out.success() => ToolOutput::ok(call_id, format!("launched {package}")),
        Ok(out) => ToolOutput::err(
            call_id,
            format!(
                "launch_app failed (is '{package}' installed?): {}",
                out.stderr
            ),
        ),
        Err(e) => ToolOutput::err(call_id, e),
    }
}

async fn force_stop(call_id: &str, serial: &str, args: &Value, declared: &[String]) -> ToolOutput {
    let package = match args.get("package").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ToolOutput::err(call_id, "missing required parameter 'package'"),
    };
    if !adb::valid_package_name(package) {
        return ToolOutput::err(
            call_id,
            format!("'{package}' is not a valid Android package name"),
        );
    }
    let package = &match resolve_package_or_refuse(serial, package, declared).await {
        Ok(p) => p,
        Err(e) => return ToolOutput::err(call_id, e),
    };
    match adb::run(
        serial,
        &["shell", "am", "force-stop", package],
        adb::DEFAULT_TIMEOUT_SECS,
    )
    .await
    {
        Ok(_) => ToolOutput::ok(call_id, format!("force-stopped {package}")),
        Err(e) => ToolOutput::err(call_id, e),
    }
}

async fn list_packages(call_id: &str, serial: &str, filter: Option<&str>) -> ToolOutput {
    let out = match adb::run(
        serial,
        &["shell", "pm", "list", "packages"],
        adb::DEFAULT_TIMEOUT_SECS,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => return ToolOutput::err(call_id, e),
    };
    let text = out.stdout_text();
    let mut packages: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("package:"))
        .filter(|p| filter.is_none_or(|f| p.contains(f)))
        .collect();
    packages.sort_unstable();
    ToolOutput::ok(call_id, packages.join("\n"))
}

/// Prefix, in order, that names the foreground activity across Android
/// versions/OEM ROMs: `topResumedActivity` is current (API 29+, confirmed on
/// a real MIUI device where `dumpsys window windows` carries no
/// `mCurrentFocus` line at all), `mResumedActivity`/`mFocusedActivity` cover
/// older releases, and the window-manager dump's `mCurrentFocus`/
/// `mFocusedApp` are the last-resort fallback this originally shipped with.
const FOCUS_LINE_PREFIXES: &[&str] =
    &["topResumedActivity", "mResumedActivity", "mFocusedActivity"];

async fn current_app(call_id: &str, serial: &str) -> ToolOutput {
    let activities = match adb::run(
        serial,
        &["shell", "dumpsys", "activity", "activities"],
        adb::DEFAULT_TIMEOUT_SECS,
    )
    .await
    {
        Ok(o) => o.stdout_text(),
        Err(e) => return ToolOutput::err(call_id, e),
    };
    if let Some(line) = find_focus_line(&activities, FOCUS_LINE_PREFIXES) {
        return ToolOutput::ok(call_id, line);
    }

    // Two window probes, narrowest first. `dumpsys window windows` is the
    // long-standing idiom and stays first because its output is a fraction
    // of the size - but Android 15 no longer reports the focus lines under
    // that sub-command at all (verified against a real Android 15 device:
    // zero matches there, both markers present without it), so a bare
    // `dumpsys window` is tried before giving up. Neither is a replacement
    // for the other: older devices answer the first, newer ones the second.
    for probe in [
        &["shell", "dumpsys", "window", "windows"][..],
        &["shell", "dumpsys", "window"][..],
    ] {
        let windows = match adb::run(serial, probe, adb::DEFAULT_TIMEOUT_SECS).await {
            Ok(o) => o.stdout_text(),
            Err(e) => return ToolOutput::err(call_id, e),
        };
        if let Some(line) = find_focus_line(&windows, &["mCurrentFocus", "mFocusedApp"]) {
            return ToolOutput::ok(call_id, line);
        }
    }
    ToolOutput::err(
        call_id,
        "could not determine foreground app (no topResumedActivity/mResumedActivity/\
         mFocusedActivity/mCurrentFocus/mFocusedApp line in dumpsys output)",
    )
}

fn find_focus_line(text: &str, prefixes: &[&str]) -> Option<String> {
    text.lines()
        .map(|l| l.trim_start())
        .find(|l| prefixes.iter().any(|p| l.starts_with(p)))
        .map(|l| l.trim_end().to_string())
}

async fn wait(call_id: &str, args: &Value) -> ToolOutput {
    let ms = args
        .get("ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(500)
        .min(MAX_WAIT_MS);
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    ToolOutput::ok(call_id, format!("waited {ms}ms"))
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_requires_action() {
        let t = AndroidTool::default();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
    }

    #[test]
    fn normalize_keycode_prefixes_bare_names() {
        assert_eq!(normalize_keycode("home"), "KEYCODE_HOME");
        assert_eq!(normalize_keycode("BACK"), "KEYCODE_BACK");
    }

    #[test]
    fn normalize_keycode_passes_through_existing_prefix_and_numeric() {
        assert_eq!(normalize_keycode("KEYCODE_ENTER"), "KEYCODE_ENTER");
        assert_eq!(normalize_keycode("66"), "66");
    }

    #[test]
    fn want_normalized_defaults_true() {
        assert!(want_normalized(&json!({})));
        assert!(!want_normalized(&json!({"normalized": false})));
    }

    #[tokio::test]
    async fn missing_action_is_error() {
        let t = AndroidTool::default();
        let out = t
            .execute(&ToolCall {
                id: "1".into(),
                name: "android".into(),
                args: json!({}),
            })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("action"));
    }

    #[tokio::test]
    async fn type_text_rejects_non_ascii() {
        let out = type_text(
            "1",
            "any-serial-unused-before-ascii-check",
            &json!({"text": "kod på svenska"}),
        )
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("ASCII"));
    }

    #[tokio::test]
    async fn launch_app_rejects_invalid_package() {
        let out = launch_app(
            "1",
            "any-serial-unused-before-validation",
            &json!({"package": "com.example; rm -rf /"}),
            &[],
        )
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("not a valid"));
    }

    #[tokio::test]
    async fn tap_rejects_out_of_range_normalized_coords() {
        let out = tap(
            "1",
            "any-serial-unused-before-range-check",
            &json!({"x": 1.5, "y": 0.5}),
        )
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("0.0..=1.0"));
    }

    #[tokio::test]
    async fn tap_missing_coords_is_error() {
        let out = tap("1", "unused", &json!({"x": 0.5})).await;
        assert!(out.is_error);
        assert!(out.content.contains("'y'"));
    }
}
