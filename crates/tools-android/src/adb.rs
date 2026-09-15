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
}
