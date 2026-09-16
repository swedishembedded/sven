// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Real-checkpoint integration test: exercises `GroundTool` against an
//! actually running `brain florence2 ground`. Skips cleanly (not a failure)
//! when `brain` is not on `PATH` or the florence2 checkpoint is not staged
//! locally, mirroring `sven-tools-android/tests/live_device.rs`'s hardware
//! gate - so `make test` stays green with neither present, but this still
//! runs wherever both are.
//!
//! Unit tests in `src/tool.rs` cover the subprocess/parsing machinery against
//! a fake `brain`; this is the one place a real checkpoint is exercised.

use image::{Rgb, RgbImage};
use serde_json::json;
use sven_tool_api::tool::{Tool, ToolCall};
use sven_tools_ground::{GroundConfig, GroundTool};

/// `true` when `brain` resolves on `PATH` and a florence2 checkpoint is
/// staged (`BRAIN_FLORENCE2_DIR`, per `brain`'s own `Florence2Provider::
/// from_env`).
fn brain_and_checkpoint_available() -> bool {
    let has_brain = std::process::Command::new("brain")
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let has_checkpoint = std::env::var("BRAIN_FLORENCE2_DIR")
        .ok()
        .filter(|p| !p.is_empty())
        .is_some_and(|p| std::path::Path::new(&p).join("model.safetensors").exists());
    has_brain && has_checkpoint
}

#[tokio::test]
async fn grounds_a_real_screenshot_against_the_real_checkpoint() {
    if !brain_and_checkpoint_available() {
        eprintln!("skipping: `brain` and/or BRAIN_FLORENCE2_DIR checkpoint not available");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");
    // A synthetic, non-black frame - the point is exercising the real
    // subprocess/CLI/JSON contract end to end, not asserting on grounding
    // accuracy against a photo of a real app.
    let mut img = RgbImage::new(256, 256);
    for p in img.pixels_mut() {
        *p = Rgb([180, 180, 180]);
    }
    img.save(&path).unwrap();

    let t = GroundTool::new(GroundConfig { timeout_secs: 120, ..GroundConfig::default() });
    let out = t
        .execute(&ToolCall {
            id: "1".into(),
            name: "ground".into(),
            args: json!({ "image_path": path.to_string_lossy(), "target": "a button" }),
        })
        .await;

    assert!(!out.is_error, "real ground call failed: {}", out.content);
    let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    assert!(v.get("found").is_some());
    assert!(v.get("boxes").is_some());
    assert_eq!(v["secure_screen"], false);
}
