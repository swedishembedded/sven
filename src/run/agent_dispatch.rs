// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven agent-dispatch` - the CLI entry point for whale's real
//! agent-dispatch stdio contract.
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
//! `.agents/roadmap/android-ui-test.md`'s Phase 3/4 entries for the full
//! contract this implements.
//!
//! Swedish Embedded AB implements solutions for whale-dispatched CI test
//! automation for its clients. If your team needs expertise in agent
//! dispatch contracts or HSM-driven device testing, you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::io::Read;
use std::sync::Arc;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{json, Value};

use sven_bootstrap::{dispatch_ui_test_step, UiTestDevice, UiTestDispatchOverrides};
use sven_config::Config;

/// One whale agent-dispatch request - exactly whale's
/// `SubprocessAgentDispatcher` writes to this process's stdin:
/// `{"mode", "device": {"provider_id", "device_id"} | null, "params"}`.
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
            r#"{"mode": "ui-test", "device": {"provider_id": "local", "device_id": "phone-1"}, "params": {"instruction": "Launch the demo app"}}"#,
        )
        .expect("a well-formed request must parse");
        assert_eq!(req.mode, "ui-test");
        let device = req.device.expect("device must be Some");
        assert_eq!(device.provider_id, "local");
        assert_eq!(device.device_id, "phone-1");
        assert_eq!(req.params["instruction"], "Launch the demo app");
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
        let result: Result<DispatchRequest, _> = serde_json::from_str(r#"{"device": null, "params": {}}"#);
        assert!(result.is_err(), "mode is required, not defaulted");
    }
}
