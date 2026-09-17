// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Hardware-gated integration tests: exercise a real attached Android device
//! over ADB. Skip cleanly (not a failure) when none is attached, so `make
//! test` stays green on a box with no phone plugged in - but actually drive
//! the device wherever one is present.
//!
//! Deliberately non-destructive: no `tap`/`type_text`/`launch_app` here,
//! since those can change what's on screen or start an app on whoever's
//! phone happens to be attached. `go_home` is the one state-changing call,
//! and it's idempotent.

use serde_json::json;
use sven_tool_api::tool::{Tool, ToolCall};
use sven_tools_android::adb;
use sven_tools_android::AndroidTool;

/// Returns the serial of a single ready device, or `None` (meaning: skip).
async fn ready_serial() -> Option<String> {
    let devices = adb::list_devices().await.ok()?;
    let ready: Vec<_> = devices.into_iter().filter(|d| d.state == "device").collect();
    match ready.as_slice() {
        [one] => Some(one.serial.clone()),
        _ => None,
    }
}

macro_rules! skip_without_device {
    () => {
        match ready_serial().await {
            Some(s) => s,
            None => {
                eprintln!("skipping: no single ready ADB device attached");
                return;
            }
        }
    };
}

fn call(action: &str, extra: serde_json::Value) -> ToolCall {
    let mut args = json!({ "action": action });
    if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(e.clone());
    }
    ToolCall {
        id: "live".into(),
        name: "android".into(),
        args,
    }
}

#[tokio::test]
async fn display_info_returns_a_plausible_size() {
    let serial = skip_without_device!();
    let t = AndroidTool::new(Some(serial));
    let out = t.execute(&call("display_info", json!({}))).await;
    assert!(!out.is_error, "{}", out.content);
    let (w, h) = out
        .content
        .split_once('x')
        .map(|(w, h)| (w.parse::<u32>().unwrap(), h.parse::<u32>().unwrap()))
        .expect("expected WxH");
    assert!(w > 0 && h > 0, "implausible display size: {w}x{h}");
}

#[tokio::test]
async fn go_home_succeeds() {
    let serial = skip_without_device!();
    let t = AndroidTool::new(Some(serial));
    let out = t.execute(&call("go_home", json!({}))).await;
    assert!(!out.is_error, "{}", out.content);
}

#[tokio::test]
async fn screenshot_captures_a_real_image() {
    let serial = skip_without_device!();
    let t = AndroidTool::new(Some(serial));
    let path = std::env::temp_dir().join(format!("sven_android_test_{}.png", std::process::id()));
    let out = t
        .execute(&call("screenshot", json!({"path": path.to_string_lossy()})))
        .await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.has_images(), "screenshot should return an image part");
    let meta = std::fs::metadata(&path).expect("screenshot file should exist");
    assert!(meta.len() > 1000, "screenshot file suspiciously small: {} bytes", meta.len());
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn list_packages_filters_to_matching_packages() {
    let serial = skip_without_device!();
    let t = AndroidTool::new(Some(serial));
    let out = t
        .execute(&call("list_packages", json!({"filter": "com.android"})))
        .await;
    assert!(!out.is_error, "{}", out.content);
    // The filter is a plain substring match, so every line the device
    // returns must actually contain it.
    for line in out.content.lines().filter(|l| !l.is_empty()) {
        assert!(line.to_lowercase().contains("com.android"), "unexpected package: {line}");
    }
}

#[tokio::test]
async fn current_app_reports_a_focus_line() {
    let serial = skip_without_device!();
    let t = AndroidTool::new(Some(serial));
    let out = t.execute(&call("current_app", json!({}))).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(!out.content.is_empty());
}

#[tokio::test]
async fn wait_action_actually_waits() {
    let serial = skip_without_device!();
    let t = AndroidTool::new(Some(serial));
    let start = std::time::Instant::now();
    let out = t.execute(&call("wait", json!({"ms": 200}))).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(start.elapsed().as_millis() >= 190, "wait returned too early");
}

#[tokio::test]
async fn unknown_device_serial_is_a_clear_error() {
    let t = AndroidTool::new(Some("definitely-not-a-real-serial".to_string()));
    let out = t.execute(&call("display_info", json!({}))).await;
    assert!(out.is_error);
}

/// [`adb::resolve_serial_validated`] against a REAL `adb devices` -
/// self-skips without a single ready device, same as every other test in
/// this file. This is the real-hardware half of the device-fallback fix
/// (`sven_bootstrap::ui_test_dispatch::resolve_effective_serial`'s own unit
/// tests cover the pure decision logic against a fake device lister; this
/// proves the REAL `RealDeviceLister`/`adb devices` round trip actually
/// substitutes the sole attached device when the "requested" identity
/// (mirroring a host's own catalog device id, e.g. `"phone-1"`, sent where a
/// real ADB serial was expected - the exact bug this fix exists for) does
/// not match anything attached.
#[tokio::test]
async fn resolve_serial_validated_falls_back_to_the_real_sole_attached_device() {
    let serial = skip_without_device!();
    let pick = adb::resolve_serial_validated(&adb::RealDeviceLister, Some("phone-1"))
        .await
        .expect("a real, reachable adb must succeed");
    assert_eq!(pick, adb::SerialPick::FellBackToSole(serial));
}

/// The companion "exact match still wins" case, against the same real
/// device.
#[tokio::test]
async fn resolve_serial_validated_uses_a_real_exact_match_directly() {
    let serial = skip_without_device!();
    let pick = adb::resolve_serial_validated(&adb::RealDeviceLister, Some(serial.as_str()))
        .await
        .expect("a real, reachable adb must succeed");
    assert_eq!(pick, adb::SerialPick::Resolved(serial));
}

/// The package analogue of the two serial tests above: proves the REAL
/// `RealPackageLister`/`pm list packages` round trip actually feeds
/// `pick_package`. Deliberately device-agnostic - it asks the device what it
/// has and then resolves the first answer exactly, so it pins the round trip
/// rather than any particular app being installed.
#[tokio::test]
async fn resolve_package_validated_round_trips_through_a_real_device() {
    let serial = skip_without_device!();
    let installed = adb::PackageLister::list_packages(&adb::RealPackageLister, &serial)
        .await
        .expect("a real, reachable adb must list packages");
    let first = installed.first().expect("a real device has packages installed").clone();
    let pick = adb::resolve_package_validated(&adb::RealPackageLister, &serial, &first, &[])
        .await
        .expect("a real, reachable adb must succeed");
    assert_eq!(pick, adb::PackagePick::Resolved(first));
}

/// A caller's declaration takes precedence over the device, proven where it
/// is impossible to fake: the declared package is NOT installed on this real
/// device, so if the declaration were being ignored (or merely merged with
/// what `pm list packages` reports) this would resolve to `NoneInstalled`.
#[tokio::test]
async fn a_declaration_outranks_what_the_real_device_reports() {
    let serial = skip_without_device!();
    let declared = vec!["com.example.declaredonly".to_string()];
    let pick =
        adb::resolve_package_validated(&adb::RealPackageLister, &serial, "declaredonly", &declared)
            .await
            .expect("a real, reachable adb must succeed");
    assert_eq!(
        pick,
        adb::PackagePick::ResolvedFromHint("com.example.declaredonly".to_string())
    );
}
