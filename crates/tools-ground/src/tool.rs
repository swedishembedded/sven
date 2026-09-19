// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`GroundTool`] - a `Tool` that asks a vision model where an element is: a
//! saved screenshot PNG and a short target phrase in, a normalized bounding
//! box out (`{found, boxes: [{phrase, bbox}]}`).
//!
//! sven owns the CONTRACT here, not any particular model. [`GroundBackend`]
//! is the seam, and this crate names no vendor type; the default
//! [`SubprocessGroundBackend`] satisfies the contract by invoking a
//! configurable grounding CLI:
//!
//! ```text
//! <command> <model> ground --json --target "<target>" --in image=<path>
//! ```
//!
//! answering on stdout with the `{found, boxes}` JSON above. `command` and
//! `model` are both configuration ([`GroundConfig`]), so any tool
//! implementing that shape can serve it. A host that already holds a vision
//! model resident should implement [`GroundBackend`] directly instead - see
//! that trait's own doc for why.
//!
//! A subprocess is deliberately NOT the same path `sven-model` uses for chat
//! completions: that transport streams free text into a `ResponseEvent`, not
//! the structured `{found, boxes}` shape this tool needs, and machines never
//! call a `ModelProvider` directly anyway - only `Effect::CallTool`.
//!
//! Swedish Embedded AB implements solutions for on-device UI-test grounding
//! for its clients. If your team needs expertise in vision-model-backed test
//! automation, you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;
use tracing::debug;

use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};

use sven_image::is_flag_secure_black;

/// How many bytes of stderr to quote back in an error message.
const STDERR_TAIL_BYTES: usize = 512;

/// How [`SubprocessGroundBackend`] invokes a grounding CLI.
///
/// Every field is configuration, not architecture: point `command` at any
/// executable satisfying the CLI contract in this module's doc and the tool
/// works unchanged. The default is one known-good implementation, not a
/// requirement - and an embedder that bypasses the subprocess entirely
/// implements [`GroundBackend`] instead, where none of this applies.
///
/// Deliberately env-var configured rather than added to `sven-config`'s
/// schema: this tool is scoped to the android-ui-test workstream and does
/// not yet have a settled place in the general config surface (see the
/// roadmap's Phase 3 entry) - three env vars are the same footprint
/// `AndroidTool::default()`'s `SVEN_ANDROID_SERIAL` already uses for the
/// device serial.
#[derive(Debug, Clone, PartialEq)]
pub struct GroundConfig {
    /// Executable invoked for grounding. Must accept the `<command> <model>
    /// ground --json` shape documented in this module's doc; override with
    /// `SVEN_GROUND_COMMAND`.
    pub command: String,
    /// Model/architecture id passed as the command's first argument, e.g.
    /// `"florence2"`. How that id resolves to weights is the grounding
    /// command's business, not sven's; override with `SVEN_GROUND_MODEL`.
    pub model: String,
    /// Hard timeout for a single grounding subprocess, in seconds.
    pub timeout_secs: u64,
}

impl Default for GroundConfig {
    fn default() -> Self {
        Self {
            command: std::env::var("SVEN_GROUND_COMMAND").unwrap_or_else(|_| "brain".to_string()),
            model: std::env::var("SVEN_GROUND_MODEL").unwrap_or_else(|_| "florence2".to_string()),
            timeout_secs: std::env::var("SVEN_GROUND_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(30),
        }
    }
}

/// How a [`GroundTool`] actually reaches a grounding model.
///
/// The seam that lets a HOST process supply an in-process, already-resident
/// model instead of paying a cold start per call. This crate depends on no
/// model implementation and this trait names no vendor type: an embedder
/// implements it over whatever it already holds.
///
/// [`SubprocessGroundBackend`] is the default and keeps sven standalone -
/// one CLI invocation per call, which re-imports the checkpoint every time.
/// A long-running embedder should implement this instead: re-loading a
/// multi-hundred-megabyte checkpoint per screen dominates the actual
/// inference by an order of magnitude.
#[async_trait]
pub trait GroundBackend: Send + Sync {
    /// Locate `target` in the image at `image_path`.
    ///
    /// Returns the grounding answer as `{"found": bool, "boxes": [{"phrase",
    /// "bbox"}]}` with `bbox` normalized `[x0,y0,x1,y1]` in `[0,1]` - the
    /// shape [`GroundTool`]'s own callers parse.
    ///
    /// # Errors
    ///
    /// A human-readable message when the model could not be reached or its
    /// answer could not be understood. Never an "empty answer" stand-in for
    /// a failure - a caller retries on error and must be able to tell the
    /// two apart.
    async fn ground(&self, image_path: &Path, target: &str) -> Result<Value, String>;
}

/// The default [`GroundBackend`]: one `<command> <model> ground` subprocess
/// per call, per this module's documented CLI contract. Keeps sven usable
/// with nothing but a conforming grounding binary on `PATH`.
pub struct SubprocessGroundBackend {
    cfg: GroundConfig,
}

impl SubprocessGroundBackend {
    #[must_use]
    pub fn new(cfg: GroundConfig) -> SubprocessGroundBackend {
        SubprocessGroundBackend { cfg }
    }
}

impl Default for SubprocessGroundBackend {
    fn default() -> Self {
        SubprocessGroundBackend::new(GroundConfig::default())
    }
}

#[async_trait]
impl GroundBackend for SubprocessGroundBackend {
    async fn ground(&self, image_path: &Path, target: &str) -> Result<Value, String> {
        run_ground(&self.cfg, image_path, target).await
    }
}

pub struct GroundTool {
    backend: Arc<dyn GroundBackend>,
}

impl GroundTool {
    /// A tool that shells out per call, per `cfg` - the standalone default.
    #[must_use]
    pub fn new(cfg: GroundConfig) -> Self {
        Self::with_backend(Arc::new(SubprocessGroundBackend::new(cfg)))
    }

    /// A tool backed by `backend` - the seam an embedder uses to supply an
    /// already-resident, in-process model instead of a subprocess. See
    /// [`GroundBackend`] for why that matters.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn GroundBackend>) -> Self {
        Self { backend }
    }
}

impl Default for GroundTool {
    fn default() -> Self {
        Self::new(GroundConfig::default())
    }
}

#[async_trait]
impl Tool for GroundTool {
    fn name(&self) -> &str {
        "ground"
    }

    fn description(&self) -> &str {
        "Locate a UI element or phrase in a saved screenshot. Required fields: \
         'image_path' (a PNG path, e.g. from the `android` tool's `screenshot` \
         action) and 'target' (the phrase or element to find, e.g. \"the Login \
         button\" or \"Continue\"). Returns JSON: {found, boxes: \
         [{phrase, bbox}]} with bbox normalized [x0,y0,x1,y1] in [0,1], or \
         {secure_screen: true} when the screenshot is a solid-black Android \
         FLAG_SECURE placeholder - this tool never sends such a frame to the \
         grounding model, since there is nothing in it to see."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "image_path": { "type": "string", "description": "Path to the screenshot PNG" },
                "target": { "type": "string", "description": "The phrase or UI element to locate" }
            },
            "required": ["image_path", "target"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let image_path = match call.args.get("image_path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolOutput::err(&call.id, "missing required parameter 'image_path'"),
        };
        let target = match call.args.get("target").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolOutput::err(&call.id, "missing required parameter 'target'"),
        };

        let img = match image::open(image_path) {
            Ok(i) => i,
            Err(e) => {
                return ToolOutput::err(
                    &call.id,
                    format!("could not decode screenshot at {image_path:?}: {e}"),
                )
            }
        };

        // The FLAG_SECURE check runs before anything is sent to brain - see
        // the module docs and `black_screen`'s own docs for why this is not
        // a fallback on a bad answer but a hard local gate.
        if is_flag_secure_black(&img) {
            debug!(
                image_path,
                "ground: solid-black FLAG_SECURE frame; skipping the grounding model"
            );
            return ToolOutput::ok(
                &call.id,
                json!({ "secure_screen": true, "found": false, "boxes": [] }).to_string(),
            );
        }

        match self.backend.ground(Path::new(image_path), target).await {
            Ok(value) => ToolOutput::ok(&call.id, value.to_string()),
            Err(detail) => ToolOutput::err(&call.id, detail),
        }
    }
}

impl ToolDisplay for GroundTool {
    fn display_name(&self) -> &str {
        "Ground"
    }
    fn category(&self) -> &str {
        "system"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        args.get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }
}

/// Build the `brain <arch> ground` argument list for one invocation.
fn ground_args(cfg: &GroundConfig, image_path: &Path, target: &str) -> Vec<String> {
    vec![
        cfg.model.clone(),
        "ground".to_string(),
        "--json".to_string(),
        "--target".to_string(),
        target.to_string(),
        "--in".to_string(),
        format!("image={}", image_path.display()),
    ]
}

/// Parse `brain florence2 ground --json`'s stdout into this tool's output
/// shape, adding `secure_screen: false` so every successful result - whether
/// or not it ever reached brain - has the same three keys.
fn parse_ground_stdout(stdout: &str) -> Result<Value, String> {
    let mut saw_json = false;
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        saw_json = true;
        let Some(obj) = value.as_object_mut() else {
            continue;
        };
        if !obj.contains_key("found") || !obj.contains_key("boxes") {
            continue;
        }
        obj.entry("secure_screen").or_insert(json!(false));
        return Ok(value);
    }
    if saw_json {
        Err("JSON output has no 'found'/'boxes' fields".to_string())
    } else {
        Err(format!(
            "no JSON line found in stdout ({} bytes)",
            stdout.len()
        ))
    }
}

async fn run_ground(cfg: &GroundConfig, image_path: &Path, target: &str) -> Result<Value, String> {
    let args = ground_args(cfg, image_path, target);
    debug!(command = %cfg.command, ?args, "running ground subprocess");

    let child = match Command::new(&cfg.command)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("could not run '{}': {e}", cfg.command)),
    };

    let timeout = Duration::from_secs(cfg.timeout_secs.max(1));
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => {
            return Err(format!(
                "'{}' timed out after {}s",
                cfg.command, cfg.timeout_secs
            ))
        }
        Ok(Err(e)) => return Err(format!("'{}' failed to run: {e}", cfg.command)),
        Ok(Ok(o)) => o,
    };

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(format!(
            "'{}' exited with {}: {}",
            cfg.command,
            output.status,
            tail(&stderr, STDERR_TAIL_BYTES)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    parse_ground_stdout(&stdout)
        .map_err(|detail| format!("{detail}; stderr: {}", tail(&stderr, STDERR_TAIL_BYTES)))
}

/// Return at most the last `max` bytes of `s`, on a char boundary.
fn tail(s: &str, max: usize) -> String {
    let trimmed = s.trim_end();
    if trimmed.len() <= max {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - max;
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn write_png(
        dir: &tempfile::TempDir,
        name: &str,
        rgb: [u8; 3],
        w: u32,
        h: u32,
    ) -> std::path::PathBuf {
        let mut img = RgbImage::new(w, h);
        for p in img.pixels_mut() {
            *p = Rgb(rgb);
        }
        let path = dir.path().join(name);
        img.save(&path).unwrap();
        path
    }

    /// A fake `brain` executable: a shell script that ignores its arguments
    /// and prints a fixed line of JSON to stdout - lets the subprocess path
    /// (spawn, wait, parse) be exercised without a real `brain` binary or
    /// checkpoint. Mirrors this crate's Phase-3 testing rule: unit tests run
    /// against a fake, never a real device/checkpoint.
    fn fake_brain(dir: &tempfile::TempDir, script: &str) -> String {
        let path = dir.path().join("fake-brain.sh");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh").unwrap();
        f.write_all(script.as_bytes()).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn cfg_with_command(command: String) -> GroundConfig {
        GroundConfig {
            command,
            model: "florence2".to_string(),
            timeout_secs: 5,
        }
    }

    #[tokio::test]
    async fn missing_image_path_is_error() {
        let t = GroundTool::default();
        let out = t
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "target": "x" }),
            })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("image_path"));
    }

    #[tokio::test]
    async fn missing_target_is_error() {
        let t = GroundTool::default();
        let out = t
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "image_path": "/x.png" }),
            })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("target"));
    }

    #[tokio::test]
    async fn a_missing_screenshot_file_is_a_clean_error_not_a_panic() {
        let t = GroundTool::default();
        let out = t
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "image_path": "/nonexistent/shot.png", "target": "x" }),
            })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("decode"));
    }

    /// The headline constraint: a solid-black FLAG_SECURE frame never
    /// reaches the grounding model, even when the configured `brain`
    /// command would fail or hang - it must never even be invoked.
    #[tokio::test]
    async fn a_black_screenshot_short_circuits_without_invoking_brain() {
        let dir = tempfile::tempdir().unwrap();
        let png = write_png(&dir, "black.png", [0, 0, 0], 32, 32);
        let t = GroundTool::new(cfg_with_command(
            "/nonexistent/should-never-run".to_string(),
        ));
        let out = t
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "image_path": png.to_string_lossy(), "target": "confirm on the secure screen" }),
            })
            .await;
        assert!(
            !out.is_error,
            "a detected secure screen is a result, not a failure: {}",
            out.content
        );
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["secure_screen"], true);
        assert_eq!(v["found"], false);
    }

    #[tokio::test]
    async fn a_normal_screenshot_calls_the_configured_command_and_parses_its_json() {
        let dir = tempfile::tempdir().unwrap();
        let png = write_png(&dir, "shot.png", [200, 30, 30], 32, 32);
        let brain = fake_brain(
            &dir,
            r#"echo '{"found": true, "boxes": [{"phrase": "log in", "bbox": [0.1, 0.2, 0.3, 0.4]}]}'"#,
        );
        let t = GroundTool::new(cfg_with_command(brain));
        let out = t
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "image_path": png.to_string_lossy(), "target": "log in" }),
            })
            .await;
        assert!(!out.is_error, "{}", out.content);
        let v: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["found"], true);
        assert_eq!(v["secure_screen"], false);
        assert_eq!(v["boxes"][0]["phrase"], "log in");
        assert_eq!(v["boxes"][0]["bbox"][0], 0.1);
    }

    #[tokio::test]
    async fn ground_args_pass_the_short_arch_id_target_and_image_path() {
        let cfg = cfg_with_command("brain".to_string());
        let args = ground_args(&cfg, Path::new("shot.png"), "log in with password");
        assert_eq!(
            args,
            vec![
                "florence2".to_string(),
                "ground".to_string(),
                "--json".to_string(),
                "--target".to_string(),
                "log in with password".to_string(),
                "--in".to_string(),
                "image=shot.png".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_an_error_with_stderr_quoted() {
        let dir = tempfile::tempdir().unwrap();
        let png = write_png(&dir, "shot.png", [10, 200, 10], 16, 16);
        let brain = fake_brain(&dir, "echo 'model not loaded' 1>&2\nexit 1\n");
        let t = GroundTool::new(cfg_with_command(brain));
        let out = t
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "image_path": png.to_string_lossy(), "target": "x" }),
            })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("model not loaded"));
    }

    #[tokio::test]
    async fn unparsable_stdout_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let png = write_png(&dir, "shot.png", [10, 200, 10], 16, 16);
        let brain = fake_brain(&dir, "echo 'not json at all'\n");
        let t = GroundTool::new(cfg_with_command(brain));
        let out = t
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "image_path": png.to_string_lossy(), "target": "x" }),
            })
            .await;
        assert!(out.is_error, "unparsable stdout must be an error");
        // Named, because every other way this can fail - a spawn that lost a
        // race for a process slot, a shell that never ran - also produces an
        // error, and a bare `contains` reports them all as "the parse error is
        // missing" without saying what arrived instead.
        assert!(
            out.content.contains("no JSON line"),
            "expected the parse error, got: {}",
            out.content
        );
    }

    #[test]
    fn parse_ground_stdout_defaults_secure_screen_to_false() {
        let v = parse_ground_stdout(r#"{"found": false, "boxes": []}"#).unwrap();
        assert_eq!(v["secure_screen"], false);
    }

    #[tokio::test]
    async fn a_nonexistent_command_is_a_clean_spawn_error() {
        let dir = tempfile::tempdir().unwrap();
        let png = write_png(&dir, "shot.png", [10, 200, 10], 16, 16);
        let t = GroundTool::new(cfg_with_command(
            "/nonexistent/sven-ground-binary".to_string(),
        ));
        let out = t
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "image_path": png.to_string_lossy(), "target": "x" }),
            })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("could not run"));
    }

    #[test]
    fn default_config_uses_the_short_florence2_arch_id() {
        assert_eq!(GroundConfig::default().model, "florence2");
    }

    #[tokio::test]
    async fn schema_requires_image_path_and_target() {
        let t = GroundTool::default();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("image_path")));
        assert!(required.iter().any(|v| v.as_str() == Some("target")));
    }

    // ─── GroundBackend seam ──────────────────────────────────────────────

    struct FixedBackend;
    #[async_trait]
    impl GroundBackend for FixedBackend {
        async fn ground(&self, _image_path: &Path, target: &str) -> Result<Value, String> {
            Ok(json!({
                "found": true,
                "boxes": [{ "phrase": target, "bbox": [0.1, 0.2, 0.3, 0.4] }]
            }))
        }
    }

    /// A backend that must never be reached - proves the FLAG_SECURE gate
    /// short-circuits BEFORE any model is consulted, rather than relying on
    /// the model to return nothing useful for a black frame.
    struct PanicsIfCalled;
    #[async_trait]
    impl GroundBackend for PanicsIfCalled {
        async fn ground(&self, _image_path: &Path, _target: &str) -> Result<Value, String> {
            panic!("the grounding backend must not be reached for a FLAG_SECURE frame");
        }
    }

    #[tokio::test]
    async fn an_injected_backend_is_used_instead_of_shelling_out() {
        let dir = tempfile::tempdir().unwrap();
        let png = write_png(&dir, "shot.png", [200, 30, 30], 32, 32);
        let tool = GroundTool::with_backend(Arc::new(FixedBackend));
        let out = tool
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "image_path": png.to_string_lossy(), "target": "log in" }),
            })
            .await;
        let value: Value =
            serde_json::from_str(&out.content).expect("backend JSON reaches the caller");
        assert_eq!(value["found"], json!(true));
        assert_eq!(value["boxes"][0]["phrase"], json!("log in"));
    }

    #[tokio::test]
    async fn a_flag_secure_frame_never_reaches_the_backend() {
        let dir = tempfile::tempdir().unwrap();
        // Solid black is the FLAG_SECURE screencap placeholder.
        let png = write_png(&dir, "secure.png", [0, 0, 0], 64, 64);
        let tool = GroundTool::with_backend(Arc::new(PanicsIfCalled));
        let out = tool
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "image_path": png.to_string_lossy(), "target": "the code" }),
            })
            .await;
        let value: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(value["secure_screen"], json!(true));
        assert_eq!(value["found"], json!(false));
    }

    /// A backend refusal must surface as a tool error, not be swallowed into
    /// a "nothing found" answer a caller would retry blindly.
    #[tokio::test]
    async fn a_backend_error_surfaces_as_a_tool_error() {
        struct FailingBackend;
        #[async_trait]
        impl GroundBackend for FailingBackend {
            async fn ground(&self, _image_path: &Path, _target: &str) -> Result<Value, String> {
                Err("florence2 is not configured".to_string())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let png = write_png(&dir, "shot.png", [200, 30, 30], 32, 32);
        let tool = GroundTool::with_backend(Arc::new(FailingBackend));
        let out = tool
            .execute(&ToolCall {
                id: "1".into(),
                name: "ground".into(),
                args: json!({ "image_path": png.to_string_lossy(), "target": "x" }),
            })
            .await;
        assert!(out.is_error, "a backend refusal must be an error");
        assert!(
            out.content.contains("florence2 is not configured"),
            "{}",
            out.content
        );
    }
}
