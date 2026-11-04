// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Real-device, real-checkpoint integration test for
//! [`sven_bootstrap::dispatch_ui_test_step`]: proves the default (no
//! overrides) wiring actually drives a real attached Android device through
//! the real `sven-tools-android` tool implementation, not just against the
//! fakes `ui_test_dispatch`'s own unit tests use.
//!
//! Skips cleanly (not a failure) unless a single ready ADB device is
//! attached AND the live run is opted into, mirroring
//! `sven-tools-android/tests/live_device.rs`'s own hardware gate - so
//! `make test` stays green on a box with neither present. The step
//! compiler's model call still goes through whatever `sven_bootstrap::Config`
//! resolves for real (no model override): if the environment has no usable
//! model credentials, this test reports that as a skip too rather than a
//! failure, since proving a specific LLM works is out of scope here - the
//! machine-level tests already cover the step compiler against a scripted
//! model exhaustively; this test's whole point is the REAL-tool wiring.
//!
//! Compiled only with the `android` feature, which is what brings the tool and
//! the dispatch entry point in.

#![cfg(feature = "android")]

use std::sync::Arc;

use sven_bootstrap::Config;
use sven_bootstrap::{dispatch_ui_test_step, UiTestDispatchOverrides};
use sven_tools_android::adb;

/// Returns the serial of a single ready device, or `None` (meaning: skip).
/// Mirrors `sven-tools-android/tests/live_device.rs::ready_serial`.
async fn ready_serial() -> Option<String> {
    let devices = adb::list_devices().await.ok()?;
    let ready: Vec<_> = devices
        .into_iter()
        .filter(|d| d.state == "device")
        .collect();
    match ready.as_slice() {
        [one] => Some(one.serial.clone()),
        _ => None,
    }
}

/// An explicit opt-in: the step compiler's model call goes through the real
/// configuration, which a box without a usable provider cannot serve.
fn live_run_opted_in() -> bool {
    std::env::var("SVEN_UI_TEST_LIVE").is_ok_and(|v| v == "1")
}

#[tokio::test]
async fn a_real_non_destructive_step_runs_against_a_real_device_with_real_tools() {
    let Some(serial) = ready_serial().await else {
        eprintln!("skipping: no single ready ADB device attached");
        return;
    };
    if !live_run_opted_in() {
        eprintln!("skipping: set SVEN_UI_TEST_LIVE=1 to run this against a real model");
        return;
    }

    // `device_id` is the host's own stable catalog/leasing key (never a real
    // ADB serial - see `UiTestDevice`'s own doc); `serial` is the real ADB
    // identity this test resolved above. A plain `"phone-1"` catalog id here
    // (deliberately NOT the real serial) proves the exact-serial-match path
    // still finds the real device even when the catalog id itself would
    // never resolve to anything.
    let device = sven_bootstrap::UiTestDevice {
        provider_id: "local".into(),
        device_id: "phone-1".into(),
        serial: Some(serial),
        // No declared app list: this instruction drives no app, and leaving
        // it empty is the ordinary no-inventory path (see `UiTestDevice::apps`).
        apps: Vec::new(),
    };

    // "Go home" is the one instruction `sven-tools-android/tests/live_device.rs`
    // itself treats as safe/idempotent to run against whoever's real phone
    // is attached - see that file's own "deliberately non-destructive" note.
    let params = serde_json::json!({ "instruction": "Go home" });

    match dispatch_ui_test_step(
        Arc::new(Config::default()),
        Some(&device),
        &params,
        UiTestDispatchOverrides::default(),
    )
    .await
    {
        Ok(output) => assert_eq!(output["passed"], true),
        Err(e) if e.contains("model provider") || e.contains("could not build") => {
            eprintln!("skipping: no usable model credentials in this environment ({e})");
        }
        Err(e) => panic!("real device/checkpoint run failed: {e}"),
    }
}
