// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Real-device, real-checkpoint integration test for
//! [`sven_bootstrap::dispatch_ui_test_step`]: proves the default (no
//! overrides) wiring actually drives a real attached Android device through
//! the real `sven-tools-android`/`sven-tools-ground` tool implementations,
//! not just against the fakes `ui_test_dispatch`'s own unit tests use.
//!
//! Skips cleanly (not a failure) unless a single ready ADB device AND a
//! real `brain florence2` checkpoint are both available, mirroring
//! `sven-tools-android/tests/live_device.rs` and
//! `sven-tools-ground/tests/live_ground.rs`'s own hardware gates - so
//! `make test` stays green on a box with neither present. The step
//! compiler's model call still goes through whatever `sven_config::Config`
//! resolves for real (no model override): if the environment has no usable
//! model credentials, this test reports that as a skip too rather than a
//! failure, since proving a specific LLM works is out of scope here - the
//! machine-level tests already cover the step compiler against a scripted
//! model exhaustively; this test's whole point is the REAL-tool wiring.

use std::sync::Arc;

use sven_bootstrap::{dispatch_ui_test_step, UiTestDispatchOverrides};
use sven_config::Config;
use sven_tools_android::adb;

/// Returns the serial of a single ready device, or `None` (meaning: skip).
/// Mirrors `sven-tools-android/tests/live_device.rs::ready_serial`.
async fn ready_serial() -> Option<String> {
    let devices = adb::list_devices().await.ok()?;
    let ready: Vec<_> = devices.into_iter().filter(|d| d.state == "device").collect();
    match ready.as_slice() {
        [one] => Some(one.serial.clone()),
        _ => None,
    }
}

/// Mirrors `sven-tools-ground/tests/live_ground.rs::brain_and_checkpoint_available`.
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
async fn a_real_non_destructive_step_runs_against_a_real_device_with_real_tools() {
    let Some(serial) = ready_serial().await else {
        eprintln!("skipping: no single ready ADB device attached");
        return;
    };
    if !brain_and_checkpoint_available() {
        eprintln!("skipping: `brain` and/or BRAIN_FLORENCE2_DIR checkpoint not available");
        return;
    }

    let device = sven_bootstrap::UiTestDevice {
        provider_id: "local".into(),
        device_id: serial,
    };

    // "Go home" is the one instruction `sven-tools-android/tests/live_device.rs`
    // itself treats as safe/idempotent to run against whoever's real phone
    // is attached - see that file's own "deliberately non-destructive" note.
    let params = serde_json::json!({ "instruction": "Go home" });

    match dispatch_ui_test_step(Arc::new(Config::default()), Some(&device), &params, UiTestDispatchOverrides::default()).await {
        Ok(output) => assert_eq!(output["passed"], true),
        Err(e) if e.contains("model provider") || e.contains("could not build") => {
            eprintln!("skipping: no usable model credentials in this environment ({e})");
        }
        Err(e) => panic!("real device/checkpoint run failed: {e}"),
    }
}
