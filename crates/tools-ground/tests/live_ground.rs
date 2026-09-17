// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Real-model integration test: exercises `GroundTool` against an actually
//! running grounding command, whichever one `SVEN_GROUND_COMMAND` names.
//! Skips cleanly (not a failure) unless it is opted into, mirroring
//! `sven-tools-android/tests/live_device.rs`'s hardware gate - so `make
//! test` stays green on a machine with no grounding model at all.
//!
//! The gate is an explicit opt-in (`SVEN_GROUND_LIVE_TEST=1`) rather than a
//! sniff for some particular vendor's staged weights, and that is the
//! point: sven owns the CLI contract, not any model's installation layout,
//! so it has no business knowing which directory a given implementation
//! keeps its checkpoint in. The operator who configured the command is the
//! one who knows it is ready.
//!
//! Unit tests in `src/tool.rs` cover the subprocess/parsing machinery
//! against a fake command; this is the one place a real model is exercised.

use image::{Rgb, RgbImage};
use serde_json::json;
use sven_tool_api::tool::{Tool, ToolCall};
use sven_tools_ground::{GroundConfig, GroundTool};

/// `true` when the operator opted this test in AND the configured grounding
/// command actually resolves on `PATH`. Both halves matter: the opt-in says
/// a model is ready, the `--help` probe catches a typo'd command before it
/// shows up as a confusing grounding failure.
pub fn live_grounding_opted_in() -> bool {
    let opted_in = std::env::var("SVEN_GROUND_LIVE_TEST")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    if !opted_in {
        return false;
    }
    let command = GroundConfig::default().command;
    std::process::Command::new(&command)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[tokio::test]
async fn grounds_a_real_screenshot_against_the_real_checkpoint() {
    if !live_grounding_opted_in() {
        eprintln!(
            "skipping: set SVEN_GROUND_LIVE_TEST=1 (and SVEN_GROUND_COMMAND/\
             SVEN_GROUND_MODEL if the defaults are not what you want) to run this"
        );
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

    let t = GroundTool::new(GroundConfig {
        timeout_secs: 120,
        ..GroundConfig::default()
    });
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
