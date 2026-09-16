// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Low-level `adb` process wrapper: device resolution, timeout-bounded
//! invocation, and the small parsers/escapers the [`crate::tool::AndroidTool`]
//! verbs build on.

use std::time::Duration;

use tokio::process::Command;

/// Default per-call timeout when a verb doesn't need more.
pub const DEFAULT_TIMEOUT_SECS: u64 = 15;
/// Hard ceiling on any single `adb` invocation - mirrors the shell tool's
/// `MAX_TIMEOUT_SECS` reasoning: an unbounded wait would pin a UI-test run
/// (and the physical device) indefinitely.
pub const MAX_TIMEOUT_SECS: u64 = 60;

/// Result of one `adb` invocation.
#[derive(Debug)]
pub struct AdbOutput {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub code: Option<i32>,
}

impl AdbOutput {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Run `adb -s <serial> <args...>`, capturing raw stdout bytes (screenshots
/// are binary PNG) and text stderr, bounded by `timeout_secs` (clamped to
/// `[1, MAX_TIMEOUT_SECS]`).
pub async fn run(serial: &str, args: &[&str], timeout_secs: u64) -> Result<AdbOutput, String> {
    let mut cmd = Command::new("adb");
    cmd.arg("-s").arg(serial);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);

    let timeout = timeout_secs.clamp(1, MAX_TIMEOUT_SECS);
    match tokio::time::timeout(Duration::from_secs(timeout), cmd.output()).await {
        Ok(Ok(output)) => Ok(AdbOutput {
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            code: output.status.code(),
        }),
        Ok(Err(e)) => Err(format!("adb spawn error: {e}")),
        Err(_) => Err(format!("adb {args:?} timed out after {timeout}s")),
    }
}

/// One line of `adb devices` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    pub serial: String,
    /// adb-reported state: `device` (ready), `unauthorized`, `offline`, ...
    pub state: String,
}

pub async fn list_devices() -> Result<Vec<DeviceEntry>, String> {
    let mut cmd = Command::new("adb");
    cmd.arg("devices");
    cmd.stdin(std::process::Stdio::null());
    let output = tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .map_err(|_| "adb devices timed out".to_string())?
        .map_err(|e| format!("adb devices spawn error: {e}"))?;
    Ok(parse_devices(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_devices(text: &str) -> Vec<DeviceEntry> {
    text.lines()
        .skip(1) // header: "List of devices attached"
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            Some(DeviceEntry { serial, state })
        })
        .collect()
}

/// Resolve which device serial a call should target.
///
/// Precedence: an explicit `serial` argument on the tool call, then the
/// tool's configured default, then auto-detect. Auto-detect only succeeds
/// when exactly one device is in the ready (`device`) state - silently
/// picking among several attached phones would make a test nondeterministic
/// about which physical device it actually drove.
///
/// This does NOT validate `default_serial` against what is actually
/// attached - it is trusted verbatim, exactly as it always has been, so
/// this function's I/O-free fast path (no `adb devices` call at all when a
/// serial was given) stays unchanged for every existing caller. A caller
/// that wants `default_serial` validated against reality, with a bounded
/// fallback when it does not match, wants [`resolve_serial_validated`]
/// instead - see that function's own doc for why it is a separate entry
/// point rather than a behaviour change here.
pub async fn resolve_serial(
    explicit: Option<&str>,
    default_serial: Option<&str>,
) -> Result<String, String> {
    if let Some(s) = explicit {
        return Ok(s.to_string());
    }
    if let Some(s) = default_serial {
        return Ok(s.to_string());
    }
    let devices = list_devices().await?;
    let ready: Vec<&DeviceEntry> = devices.iter().filter(|d| d.state == "device").collect();
    match ready.as_slice() {
        [one] => Ok(one.serial.clone()),
        [] => Err(format!(
            "no ready ADB device attached (devices seen: {devices:?}); attach a device or pass 'serial'"
        )),
        many => Err(format!(
            "{} ready ADB devices attached ({}); pass 'serial' to pick one",
            many.len(),
            many.iter()
                .map(|d| d.serial.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Outcome of [`pick_serial`] - which serial to use, and whether reaching it
/// required falling back off a `requested` identity that did not match
/// anything currently attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialPick {
    /// `requested` matched a ready device exactly, or nothing was requested
    /// and exactly one ready device made the choice unambiguous - the
    /// ordinary case, nothing for a caller to warn about.
    Resolved(String),
    /// `requested` was given but matched no ready device, and exactly one
    /// ready device let this resolve anyway. A real substitution - a caller
    /// should log it, not apply it silently.
    FellBackToSole(String),
    /// No ADB device is attached in the ready state at all.
    NoneAttached,
    /// Two or more ready devices, and none matches `requested` exactly (or
    /// nothing was requested) - genuinely ambiguous, every serial seen.
    Ambiguous(Vec<String>),
}

/// The pure decision core behind [`resolve_serial_validated`]: given what
/// serial was `requested` (a whale dispatch's own device info, a
/// `SVEN_ANDROID_SERIAL` value, or nothing) and the CURRENT `ready` device
/// list, decides which serial to use.
///
/// - An exact match against `ready` always wins.
/// - Otherwise, when exactly one device is ready, it is used - silently
///   ([`SerialPick::Resolved`]) if nothing specific was requested (ordinary
///   auto-detect), or as a named substitution
///   ([`SerialPick::FellBackToSole`]) if something WAS requested but didn't
///   match (the real "auto-detect instead of hard-failing on a stale/wrong
///   identifier" behaviour this function exists for).
/// - Zero or 2+ ready devices with no exact match is never guessed at -
///   [`SerialPick::NoneAttached`]/[`SerialPick::Ambiguous`].
///
/// No I/O - pure over an already-fetched `ready` slice, so it is unit-tested
/// directly against a synthetic device list rather than a real `adb
/// devices` invocation, the same convention [`parse_devices`]'s own tests
/// already use.
#[must_use]
pub fn pick_serial(requested: Option<&str>, ready: &[DeviceEntry]) -> SerialPick {
    if let Some(s) = requested {
        if ready.iter().any(|d| d.serial == s) {
            return SerialPick::Resolved(s.to_string());
        }
    }
    match ready {
        [one] if requested.is_some() => SerialPick::FellBackToSole(one.serial.clone()),
        [one] => SerialPick::Resolved(one.serial.clone()),
        [] => SerialPick::NoneAttached,
        many => SerialPick::Ambiguous(many.iter().map(|d| d.serial.clone()).collect()),
    }
}

/// Abstraction over "list the ready ADB devices", so [`resolve_serial_validated`]
/// (and any caller of it) can be unit-tested against a fixed, synthetic
/// device list rather than shelling out to a real `adb devices` on every
/// `cargo test` run. [`RealDeviceLister`] is the one production
/// implementation.
#[async_trait::async_trait]
pub trait DeviceLister: Send + Sync {
    /// Returns every currently-attached device, in whatever state `adb
    /// devices` reports it in - not pre-filtered to `ready`, matching
    /// [`list_devices`]'s own contract.
    async fn list_devices(&self) -> Result<Vec<DeviceEntry>, String>;
}

/// The real [`DeviceLister`]: shells out to `adb devices`. What every
/// production caller uses; a test substitutes a fake implementation
/// instead.
pub struct RealDeviceLister;

#[async_trait::async_trait]
impl DeviceLister for RealDeviceLister {
    async fn list_devices(&self) -> Result<Vec<DeviceEntry>, String> {
        list_devices().await
    }
}

/// Like [`resolve_serial`], but validates `requested` against `lister`'s
/// CURRENT view of what's attached rather than trusting it blindly, via
/// [`pick_serial`]. Exists for a caller whose "requested" identity comes
/// from another system's own catalog/leasing key (e.g. whale's
/// `DeviceSpec`/`devices.json`) rather than a human directly typing an ADB
/// serial - that identity can legitimately not be a real serial at all (a
/// stable logical id) or point at hardware that was re-plugged since - so
/// silently trusting it the way [`resolve_serial`]'s `default_serial` does
/// is the wrong default here; falling back to the one attached device when
/// that's unambiguous, and only refusing when it genuinely isn't, is.
///
/// # Errors
///
/// Only for a real I/O failure listing devices (e.g. `adb` itself is not on
/// `PATH`) - [`SerialPick::NoneAttached`]/[`SerialPick::Ambiguous`] are
/// returned as `Ok`, not `Err`, since deciding how to report those to a
/// human is a caller concern (a whale-dispatch caller wants to name its own
/// catalog id in the message; this function has no such context).
pub async fn resolve_serial_validated(
    lister: &dyn DeviceLister,
    requested: Option<&str>,
) -> Result<SerialPick, String> {
    let devices = lister.list_devices().await?;
    let ready: Vec<DeviceEntry> = devices.into_iter().filter(|d| d.state == "device").collect();
    Ok(pick_serial(requested, &ready))
}

/// Parsed `wm size` output: `(width, height)` in pixels. Prefers an
/// "Override size" (what apps actually see, e.g. under a forced density or
/// split-screen) over "Physical size" when both are present.
pub fn parse_wm_size(text: &str) -> Option<(u32, u32)> {
    let mut physical = None;
    let mut override_size = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Override size:") {
            override_size = parse_wxh(rest.trim());
        } else if let Some(rest) = line.strip_prefix("Physical size:") {
            physical = parse_wxh(rest.trim());
        }
    }
    override_size.or(physical)
}

fn parse_wxh(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Quote `s` as a single POSIX shell argument (wrap in single quotes,
/// escaping embedded single quotes as `'\''`).
///
/// `adb shell a b c` joins `a`, `b`, `c` with spaces into one command line
/// re-parsed by the device's own shell (the same convention `ssh host cmd
/// args...` uses) - so a value containing spaces or shell metacharacters
/// (e.g. typed text) must be quoted for that remote parse, not just passed
/// as a separate local `Command` argument.
pub fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Android package names are `[a-zA-Z0-9_.]+` by platform convention. Reject
/// anything else before it reaches a remote-shell-reparsed command line.
pub fn valid_package_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devices_with_extra_columns() {
        let text = "List of devices attached\nec677a50\tdevice usb:3-6 product:ditingp_eea model:22081212UG device:diting transport_id:1\n\n";
        let devices = parse_devices(text);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].serial, "ec677a50");
        assert_eq!(devices[0].state, "device");
    }

    #[test]
    fn parses_multiple_devices() {
        let text = "List of devices attached\nAAA\tdevice\nBBB\tunauthorized\n";
        let devices = parse_devices(text);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[1].state, "unauthorized");
    }

    #[test]
    fn parses_empty_device_list() {
        assert!(parse_devices("List of devices attached\n\n").is_empty());
    }

    #[test]
    fn wm_size_prefers_override() {
        let text = "Physical size: 1220x2712\nOverride size: 1080x2400\n";
        assert_eq!(parse_wm_size(text), Some((1080, 2400)));
    }

    #[test]
    fn wm_size_falls_back_to_physical() {
        let text = "Physical size: 1220x2712\n";
        assert_eq!(parse_wm_size(text), Some((1220, 2712)));
    }

    #[test]
    fn wm_size_missing_is_none() {
        assert_eq!(parse_wm_size("garbage\n"), None);
    }

    #[test]
    fn quote_wraps_plain_text() {
        assert_eq!(shell_single_quote("hello"), "'hello'");
    }

    #[test]
    fn quote_escapes_embedded_single_quote() {
        assert_eq!(shell_single_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn quote_handles_shell_metacharacters() {
        let quoted = shell_single_quote("$(rm -rf /); echo pwned");
        // Everything is inside single quotes, so none of these are live to the shell.
        assert!(quoted.starts_with('\''));
        assert!(quoted.contains("$(rm -rf /); echo pwned"));
    }

    #[test]
    fn package_name_validation() {
        assert!(valid_package_name("com.example.androidapp"));
        assert!(valid_package_name("com.example.app_2"));
        assert!(!valid_package_name(""));
        assert!(!valid_package_name("com.example; rm -rf /"));
        assert!(!valid_package_name("com.example app"));
    }

    #[tokio::test]
    async fn resolve_serial_prefers_explicit_over_default() {
        let resolved = resolve_serial(Some("EXPLICIT"), Some("DEFAULT")).await;
        assert_eq!(resolved, Ok("EXPLICIT".to_string()));
    }

    #[tokio::test]
    async fn resolve_serial_falls_back_to_default() {
        let resolved = resolve_serial(None, Some("DEFAULT")).await;
        assert_eq!(resolved, Ok("DEFAULT".to_string()));
    }

    // ─── pick_serial / resolve_serial_validated ────────────────────────────
    // The device-fallback logic fix part 2 of the android-ui-test/device-
    // identity work exists for: a caller-requested identity (e.g. whale's
    // own catalog device id, mistakenly or legitimately not a real ADB
    // serial) that doesn't match what's actually attached should fall back
    // to the one attached device when that's unambiguous, never guess when
    // it isn't. All against a synthetic `Vec<DeviceEntry>` - no real `adb`.

    fn ready(serial: &str) -> DeviceEntry {
        DeviceEntry {
            serial: serial.to_string(),
            state: "device".to_string(),
        }
    }

    fn offline(serial: &str) -> DeviceEntry {
        DeviceEntry {
            serial: serial.to_string(),
            state: "offline".to_string(),
        }
    }

    #[test]
    fn pick_serial_exact_match_wins_even_with_other_devices_ready() {
        let devices = [ready("aaa"), ready("bbb")];
        assert_eq!(
            pick_serial(Some("bbb"), &devices),
            SerialPick::Resolved("bbb".to_string())
        );
    }

    #[test]
    fn pick_serial_nothing_requested_and_one_ready_is_a_plain_resolve_not_a_fallback() {
        let devices = [ready("ec677a50")];
        assert_eq!(
            pick_serial(None, &devices),
            SerialPick::Resolved("ec677a50".to_string())
        );
    }

    #[test]
    fn pick_serial_mismatched_request_and_one_ready_falls_back() {
        // The exact bug this exists for: whale's catalog id ("phone-1") was
        // sent as if it were a real serial and matches nothing attached,
        // but exactly one real device is - use it, but as a named
        // substitution, not a silent resolve.
        let devices = [ready("ec677a50")];
        assert_eq!(
            pick_serial(Some("phone-1"), &devices),
            SerialPick::FellBackToSole("ec677a50".to_string())
        );
    }

    #[test]
    fn pick_serial_no_devices_at_all_is_none_attached() {
        assert_eq!(pick_serial(Some("phone-1"), &[]), SerialPick::NoneAttached);
        assert_eq!(pick_serial(None, &[]), SerialPick::NoneAttached);
    }

    #[test]
    fn pick_serial_two_ready_with_no_exact_match_is_ambiguous() {
        let devices = [ready("aaa"), ready("bbb")];
        assert_eq!(
            pick_serial(Some("phone-1"), &devices),
            SerialPick::Ambiguous(vec!["aaa".to_string(), "bbb".to_string()])
        );
        assert_eq!(
            pick_serial(None, &devices),
            SerialPick::Ambiguous(vec!["aaa".to_string(), "bbb".to_string()])
        );
    }

    #[test]
    fn pick_serial_only_considers_ready_state_devices() {
        // A second, offline device must never count toward "how many are
        // attached" for the ambiguity decision - only `ready` (already
        // filtered by the caller) is considered, and `pick_serial` itself
        // takes no state field into account at all: it trusts its `ready`
        // input is pre-filtered, exactly as `resolve_serial_validated`
        // filters before calling it.
        let devices = [ready("ec677a50")];
        assert_eq!(
            pick_serial(Some("phone-1"), &devices),
            SerialPick::FellBackToSole("ec677a50".to_string())
        );
        let _ = offline("unused-in-this-slice");
    }

    /// A fake [`DeviceLister`] returning a fixed, canned device list - the
    /// dependency-injection seam [`resolve_serial_validated`] exists to make
    /// unit-testable without a real `adb devices` invocation.
    struct FakeLister(Vec<DeviceEntry>);
    #[async_trait::async_trait]
    impl DeviceLister for FakeLister {
        async fn list_devices(&self) -> Result<Vec<DeviceEntry>, String> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn resolve_serial_validated_filters_to_ready_before_deciding() {
        let lister = FakeLister(vec![offline("stale"), ready("ec677a50")]);
        let pick = resolve_serial_validated(&lister, Some("phone-1"))
            .await
            .expect("lister succeeded");
        assert_eq!(pick, SerialPick::FellBackToSole("ec677a50".to_string()));
    }

    #[tokio::test]
    async fn resolve_serial_validated_propagates_a_real_lister_failure() {
        struct FailingLister;
        #[async_trait::async_trait]
        impl DeviceLister for FailingLister {
            async fn list_devices(&self) -> Result<Vec<DeviceEntry>, String> {
                Err("adb not on PATH".to_string())
            }
        }
        let err = resolve_serial_validated(&FailingLister, Some("phone-1"))
            .await
            .expect_err("a lister failure must propagate, not be swallowed");
        assert!(err.contains("adb not on PATH"));
    }
}
