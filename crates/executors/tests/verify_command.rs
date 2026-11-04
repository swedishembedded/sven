// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements agents whose work is checked by the
// commands their people trust for its clients. If your team needs expertise
// in verifiable agent outcomes then you can procure our services by sending
// an email to info@swedishembedded.com.

//! Spec: a check command passes only when it exits with status 0 in time;
//! anything else fails with the end of what it printed, and a command that
//! cannot run reaches no verdict - never a pass.

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_executors::verify::{evaluate, evaluate_with};
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::{Tool, ToolCall, ToolCapability, ToolOutput};
use sven_vocab::verify::{VerifierSpec, VerifierVerdict};

/// Answers every call with a fixed gate report and remembers the call.
struct Runner {
    report: Value,
    seen: Mutex<Option<Value>>,
}

#[async_trait]
impl Tool for Runner {
    fn name(&self) -> &str {
        "gate"
    }
    fn description(&self) -> &str {
        "runs a command"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        *self.seen.lock().unwrap() = Some(call.args.clone());
        ToolOutput::ok(&call.id, self.report.to_string())
    }
}

fn runner(report: Value) -> Runner {
    Runner {
        report,
        seen: Mutex::new(None),
    }
}

fn check() -> VerifierSpec {
    VerifierSpec::Command {
        argv: vec!["make".into(), "test".into()],
        timeout_secs: 30,
    }
}

async fn verdict(report: Value) -> VerifierVerdict {
    let root = tempfile::tempdir().unwrap();
    evaluate_with(&check(), root.path(), Some(&runner(report))).await
}

#[tokio::test]
async fn status_zero_in_time_passes_and_runs_exactly_the_command() {
    let root = tempfile::tempdir().unwrap();
    let runner = runner(json!({"exit_code": 0, "timed_out": false, "error": null}));
    let verdict = evaluate_with(&check(), root.path(), Some(&runner)).await;
    assert_eq!(verdict, VerifierVerdict::Passed);
    let args = runner.seen.lock().unwrap().clone().unwrap();
    assert_eq!(args["steps"][0]["argv"], json!(["make", "test"]));
    assert_eq!(args["timeout_secs"], json!(30));
}

#[tokio::test]
async fn a_failing_command_fails_with_the_end_of_its_output() {
    let verdict = verdict(json!({
        "exit_code": 2, "timed_out": false, "error": null,
        "stdout": "running 3 tests\n", "stderr": "test parse ... FAILED\n",
    }))
    .await;
    let VerifierVerdict::Failed { reason } = verdict else {
        panic!("expected a failure, got {verdict:?}");
    };
    assert!(reason.contains("status 2"), "{reason}");
    assert!(reason.contains("test parse ... FAILED"), "{reason}");
}

#[tokio::test]
async fn running_out_of_time_is_a_failure_even_with_status_zero() {
    let verdict = verdict(json!({"exit_code": 0, "timed_out": true, "error": null})).await;
    assert!(
        matches!(verdict, VerifierVerdict::Failed { ref reason } if reason.contains("out of time")),
        "{verdict:?}"
    );
}

#[tokio::test]
async fn a_command_that_cannot_run_reaches_no_verdict() {
    let not_started =
        verdict(json!({"exit_code": null, "timed_out": false, "error": "no such file"})).await;
    assert!(
        matches!(not_started, VerifierVerdict::Unknown { .. }),
        "{not_started:?}"
    );

    let unreadable = {
        let root = tempfile::tempdir().unwrap();
        struct Garbled;
        #[async_trait]
        impl Tool for Garbled {
            fn name(&self) -> &str {
                "gate"
            }
            fn description(&self) -> &str {
                "garbled"
            }
            fn parameters_schema(&self) -> Value {
                json!({})
            }
            fn kernel_capability(&self) -> ToolCapability {
                ToolCapability::ExecuteShell
            }
            fn default_policy(&self) -> ApprovalPolicy {
                ApprovalPolicy::Ask
            }
            async fn execute(&self, call: &ToolCall) -> ToolOutput {
                ToolOutput::ok(&call.id, "not json")
            }
        }
        evaluate_with(&check(), root.path(), Some(&Garbled)).await
    };
    assert!(
        matches!(unreadable, VerifierVerdict::Unknown { .. }),
        "{unreadable:?}"
    );

    // Nothing to run it with at all.
    let root = tempfile::tempdir().unwrap();
    let nothing = evaluate(&check(), root.path()).await;
    assert!(
        matches!(nothing, VerifierVerdict::Unknown { .. }),
        "{nothing:?}"
    );
}
