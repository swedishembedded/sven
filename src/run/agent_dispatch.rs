// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven agent-dispatch` - the CLI entry point for the agent-dispatch
//! stdio contract an orchestrating host drives sven through.
//!
//! One process per node dispatch (`sh -c "sven agent-dispatch"`, no extra
//! args): reads exactly one JSON request from stdin, then stdin is closed;
//! runs the matching sven machine to completion through the same
//! `RuntimeBuilder`/mode-registry path every other sven machine uses
//! (`sven_bootstrap::ui_test_dispatch::dispatch_ui_test_step` for
//! `mode: "ui-test"`); writes exactly one JSON reply as the LAST line of
//! stdout. Exit code 0 covers both a passed and a failed step - only a
//! malformed request or an internal fault (couldn't parse stdin, couldn't
//! build/join the kernel session) exits non-zero. See
//! `.agents/roadmap/android-ui-test.md` for the full contract this
//! implements.
//!
//! Swedish Embedded AB implements solutions for orchestrated, CI-dispatched
//! test automation for its clients. If your team needs expertise in agent
//! dispatch contracts or HSM-driven device testing, you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::io::Read;
use std::sync::Arc;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{json, Value};

use sven_bootstrap::{dispatch_ui_test_step, UiTestDevice, UiTestDispatchOverrides};
use sven_config::Config;

/// One agent-dispatch request - exactly what a dispatching host writes to
/// this process's stdin:
/// `{"mode", "device": {"provider_id", "device_id", "serial"} | null,
/// "params"}`. `device_id` is the host's own stable catalog/leasing key
/// (e.g. `"phone-1"`) - NOT a real ADB serial; `serial`, when the host's
/// catalog knows it, is the real physical address (`adb devices`' own
/// serial column, e.g. `"ec677a50"`). See `UiTestDevice`'s own doc
/// (`sven_bootstrap::ui_test_dispatch`) for why the two are never
/// conflated.
#[derive(Debug, Deserialize)]
struct DispatchRequest {
    mode: String,
    device: Option<DispatchDevice>,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize)]
struct DispatchDevice {
    provider_id: String,
    device_id: String,
    /// Absent (rather than an error) when the host's own catalog entry does
    /// not name a real serial - a real, expected state, not a malformed
    /// request. See `resolve_effective_serial`'s own doc
    /// (`sven_bootstrap::ui_test_dispatch`) for how that degrades to
    /// ordinary auto-detect.
    #[serde(default)]
    serial: Option<String>,
    /// The packages the host declares this device has installed. Defaults to
    /// empty (a host that keeps no app inventory) rather than being
    /// required - see `UiTestDevice::apps` for what a populated list buys.
    #[serde(default)]
    apps: Vec<String>,
}

pub(crate) async fn run_agent_dispatch_command(config: Arc<Config>) -> anyhow::Result<()> {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .context("reading the agent-dispatch request from stdin")?;

    // A request that does not even parse is a genuine subcommand-level
    // fault (malformed stdin) - propagated with `?` so the process exits
    // non-zero, per this subcommand's own contract. Anything past this
    // point is a well-formed request; every outcome from here on is
    // reported as a JSON reply on stdout with exit code 0.
    let request: DispatchRequest = serde_json::from_str(&raw)
        .with_context(|| format!("stdin is not a valid agent-dispatch request: {raw:?}"))?;

    let reply = match request.mode.as_str() {
        "ui-test" => {
            let device = request.device.map(|d| UiTestDevice {
                provider_id: d.provider_id,
                device_id: d.device_id,
                serial: d.serial,
                apps: d.apps,
            });
            match dispatch_ui_test_step(
                config,
                device.as_ref(),
                &request.params,
                UiTestDispatchOverrides::default(),
            )
            .await
            {
                Ok(output) => json!({ "ok": true, "output": output }),
                Err(error) => json!({ "ok": false, "error": error }),
            }
        }
        other => json!({ "ok": false, "error": format!("unsupported mode: {other}") }),
    };

    println!("{reply}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dispatch_request_parses_with_a_device() {
        let req: DispatchRequest = serde_json::from_str(
            r#"{"mode": "ui-test", "device": {"provider_id": "local", "device_id": "phone-1", "serial": "ec677a50"}, "params": {"instruction": "Launch the demo app"}}"#,
        )
        .expect("a well-formed request must parse");
        assert_eq!(req.mode, "ui-test");
        let device = req.device.expect("device must be Some");
        assert_eq!(device.provider_id, "local");
        assert_eq!(device.device_id, "phone-1");
        assert_eq!(device.serial, Some("ec677a50".to_string()));
        assert_eq!(req.params["instruction"], "Launch the demo app");
    }

    /// A host that declares which apps this device runs threads them through
    /// so a loose app-name hint in a step resolves against that declaration
    /// rather than against several hundred packages sniffed off the device.
    #[test]
    fn a_dispatch_request_parses_a_declared_app_list() {
        let req: DispatchRequest = serde_json::from_str(
            r#"{"mode": "ui-test", "device": {"provider_id": "local", "device_id": "phone-1", "apps": ["se.betalo.androidapp"]}, "params": {}}"#,
        )
        .expect("a declared app list must parse");
        let device = req.device.expect("device must be Some");
        assert_eq!(device.apps, vec!["se.betalo.androidapp".to_string()]);
    }

    /// `serial` is optional - the host's own catalog entry may not know a real
    /// ADB serial at all (see `DispatchDevice::serial`'s own doc), and that
    /// must parse cleanly, not be a required-field error.
    #[test]
    fn a_dispatch_request_parses_with_a_device_and_no_serial() {
        let req: DispatchRequest = serde_json::from_str(
            r#"{"mode": "ui-test", "device": {"provider_id": "local", "device_id": "phone-1"}, "params": {}}"#,
        )
        .expect("a device with no serial must still parse");
        let device = req.device.expect("device must be Some");
        assert_eq!(device.serial, None);
        // Same reasoning for `apps`: a host that tracks no app inventory is
        // an ordinary caller, not a malformed request.
        assert!(device.apps.is_empty());
    }

    #[test]
    fn a_dispatch_request_parses_with_a_null_device() {
        let req: DispatchRequest =
            serde_json::from_str(r#"{"mode": "code-review", "device": null, "params": {}}"#)
                .expect("a null device must parse as None");
        assert!(req.device.is_none());
    }

    #[test]
    fn a_dispatch_request_defaults_params_when_absent() {
        let req: DispatchRequest = serde_json::from_str(r#"{"mode": "ui-test", "device": null}"#)
            .expect("params must default rather than being required");
        assert!(req.params.is_null());
    }

    #[test]
    fn malformed_json_fails_to_parse() {
        let result: Result<DispatchRequest, _> = serde_json::from_str("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn a_request_missing_mode_fails_to_parse() {
        let result: Result<DispatchRequest, _> =
            serde_json::from_str(r#"{"device": null, "params": {}}"#);
        assert!(result.is_err(), "mode is required, not defaulted");
    }
}
