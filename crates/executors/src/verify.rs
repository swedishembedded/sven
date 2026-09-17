// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Evaluating a [`VerifierSpec`] against the real world.
//!
//! [`sven_vocab::verify`] only names what to check; this is where the check
//! actually happens - file reads, hashing, an HTTP request. Not wired into
//! the kernel yet (no `Effect::Verify` exists): this module is a standalone,
//! directly-callable evaluator, ready for the verified-task machine that
//! will drive it through the HSM effect/event plane.
//!
//! # Path jailing
//!
//! Every filesystem path in a spec resolves relative to a caller-supplied
//! `root` and is rejected if it would escape it (`..` components, or an
//! absolute path). A verifier's job is to check facts about a task's
//! sandbox, not to read arbitrary files the spec's author (possibly the
//! agent under evaluation) points it at.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use sven_vocab::verify::{JsonCmpOp, VerifierSpec, VerifierVerdict};

/// Evaluate `spec` against `root`.
///
/// Boxed/async-recursive to support [`VerifierSpec::All`]/[`VerifierSpec::Any`].
pub fn evaluate<'a>(
    spec: &'a VerifierSpec,
    root: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = VerifierVerdict> + Send + 'a>> {
    Box::pin(async move {
        match spec {
            VerifierSpec::FileExists { path, min_bytes } => {
                let Some(resolved) = jail(root, path) else {
                    return VerifierVerdict::Unknown {
                        reason: format!("path escapes root: {path}"),
                    };
                };
                match tokio::fs::metadata(&resolved).await {
                    Ok(meta) => match min_bytes {
                        Some(min) if meta.len() < *min => VerifierVerdict::Failed {
                            reason: format!(
                                "{path} is {} bytes, expected at least {min}",
                                meta.len()
                            ),
                        },
                        _ => VerifierVerdict::Passed,
                    },
                    Err(_) => VerifierVerdict::Failed {
                        reason: format!("{path} does not exist"),
                    },
                }
            }

            VerifierSpec::FileHash { path, sha256 } => {
                let Some(resolved) = jail(root, path) else {
                    return VerifierVerdict::Unknown {
                        reason: format!("path escapes root: {path}"),
                    };
                };
                match tokio::fs::read(&resolved).await {
                    Ok(bytes) => {
                        let digest = hex::encode(Sha256::digest(&bytes));
                        if digest.eq_ignore_ascii_case(sha256) {
                            VerifierVerdict::Passed
                        } else {
                            VerifierVerdict::Failed {
                                reason: format!("{path} hashes to {digest}, expected {sha256}"),
                            }
                        }
                    }
                    Err(e) => VerifierVerdict::Failed {
                        reason: format!("{path} unreadable: {e}"),
                    },
                }
            }

            VerifierSpec::JsonPredicate {
                path,
                pointer,
                op,
                value,
            } => {
                let Some(resolved) = jail(root, path) else {
                    return VerifierVerdict::Unknown {
                        reason: format!("path escapes root: {path}"),
                    };
                };
                let bytes = match tokio::fs::read(&resolved).await {
                    Ok(b) => b,
                    Err(e) => {
                        return VerifierVerdict::Failed {
                            reason: format!("{path} unreadable: {e}"),
                        }
                    }
                };
                let doc: serde_json::Value = match serde_json::from_slice(&bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        return VerifierVerdict::Unknown {
                            reason: format!("{path} is not valid JSON: {e}"),
                        }
                    }
                };
                let Some(found) = doc.pointer(pointer) else {
                    return VerifierVerdict::Failed {
                        reason: format!("{path}: pointer {pointer:?} not found"),
                    };
                };
                let passed = match op {
                    JsonCmpOp::Eq => found == value,
                    JsonCmpOp::Ne => found != value,
                    JsonCmpOp::Contains => match (found.as_str(), value.as_str()) {
                        (Some(hay), Some(needle)) => hay.contains(needle),
                        _ => false,
                    },
                };
                if passed {
                    VerifierVerdict::Passed
                } else {
                    VerifierVerdict::Failed {
                        reason: format!("{path}: {pointer} = {found}, op {op:?} {value} failed"),
                    }
                }
            }

            VerifierSpec::HttpPredicate {
                url,
                expect_status,
                body_contains,
            } => {
                let response = match reqwest::get(url).await {
                    Ok(r) => r,
                    Err(e) => {
                        return VerifierVerdict::Unknown {
                            reason: format!("{url} unreachable: {e}"),
                        }
                    }
                };
                let status = response.status().as_u16();
                if let Some(expected) = expect_status {
                    if status != *expected {
                        return VerifierVerdict::Failed {
                            reason: format!("{url} returned status {status}, expected {expected}"),
                        };
                    }
                }
                if let Some(needle) = body_contains {
                    let body = match response.text().await {
                        Ok(b) => b,
                        Err(e) => {
                            return VerifierVerdict::Unknown {
                                reason: format!("{url} body unreadable: {e}"),
                            }
                        }
                    };
                    if !body.contains(needle.as_str()) {
                        return VerifierVerdict::Failed {
                            reason: format!("{url} body did not contain {needle:?}"),
                        };
                    }
                }
                VerifierVerdict::Passed
            }

            VerifierSpec::AskHuman { question, options } => VerifierVerdict::NeedsHuman {
                question: question.clone(),
                options: options.clone(),
            },

            VerifierSpec::All { specs } => {
                for s in specs {
                    let v = evaluate(s, root).await;
                    if !matches!(v, VerifierVerdict::Passed) {
                        return v;
                    }
                }
                VerifierVerdict::Passed
            }

            VerifierSpec::Any { specs } => {
                let mut last = VerifierVerdict::Unknown {
                    reason: "Any with no sub-specs".to_string(),
                };
                for s in specs {
                    let v = evaluate(s, root).await;
                    if matches!(v, VerifierVerdict::Passed) {
                        return VerifierVerdict::Passed;
                    }
                    last = v;
                }
                last
            }

            // The whole point of Unsupported: never Passed, regardless of
            // what an older binary was asked to check.
            VerifierSpec::Unsupported => VerifierVerdict::Unknown {
                reason: "verifier spec kind is not recognized by this build".to_string(),
            },
        }
    })
}

/// Resolve `path` (from a spec) against `root`, rejecting anything that
/// would escape it. `None` on an absolute path or a `..` component.
fn jail(root: &Path, path: &str) -> Option<PathBuf> {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return None;
    }
    if candidate
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return None;
    }
    Some(root.join(candidate))
}

/// Executes `Effect::Verify` by calling [`evaluate`] against a fixed `root`
/// and posting the verdict back as `Event::VerificationComplete`.
///
/// `root` is owned by the executor (constructed once, at assembly time) for
/// the same reason `CheckpointExecutor` owns `repo_dir`: a transition cannot
/// supply a filesystem path itself without ceasing to be pure, so the
/// environment it resolves against is injected at the boundary instead.
pub struct VerifyExecutor {
    root: PathBuf,
}

impl VerifyExecutor {
    /// Creates an executor that jails every verification to `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait::async_trait]
impl sven_kernel::EffectExecutor for VerifyExecutor {
    async fn execute(
        &mut self,
        effect: sven_hsm::Effect,
        sink: &sven_kernel::EventSink,
        _obs: &sven_hsm::ObservationSink,
    ) {
        let sven_hsm::Effect::Verify { spec } = effect else {
            return;
        };
        let verdict = evaluate(&spec, &self.root).await;
        let _ = sink
            .emit(sven_hsm::Event::VerificationComplete { verdict })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_kernel::EffectExecutor;

    fn tempdir() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("tempdir")
    }

    // ── FileExists ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn file_exists_passes_when_present() {
        let dir = tempdir();
        std::fs::write(dir.path().join("out.txt"), "hello").unwrap();
        let spec = VerifierSpec::FileExists {
            path: "out.txt".into(),
            min_bytes: None,
        };
        assert_eq!(evaluate(&spec, dir.path()).await, VerifierVerdict::Passed);
    }

    #[tokio::test]
    async fn file_exists_fails_when_absent() {
        let dir = tempdir();
        let spec = VerifierSpec::FileExists {
            path: "missing.txt".into(),
            min_bytes: None,
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn file_exists_enforces_min_bytes() {
        let dir = tempdir();
        std::fs::write(dir.path().join("out.txt"), "hi").unwrap(); // 2 bytes
        let spec = VerifierSpec::FileExists {
            path: "out.txt".into(),
            min_bytes: Some(10),
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn a_path_escaping_the_root_is_unknown_not_evaluated() {
        let dir = tempdir();
        let spec = VerifierSpec::FileExists {
            path: "../../etc/passwd".into(),
            min_bytes: None,
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Unknown { .. }
        ));
    }

    #[tokio::test]
    async fn an_absolute_path_is_unknown_not_evaluated() {
        let dir = tempdir();
        let spec = VerifierSpec::FileExists {
            path: "/etc/passwd".into(),
            min_bytes: None,
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Unknown { .. }
        ));
    }

    // ── FileHash ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn file_hash_passes_on_matching_digest() {
        let dir = tempdir();
        std::fs::write(dir.path().join("out.txt"), "hello").unwrap();
        let digest = hex::encode(Sha256::digest(b"hello"));
        let spec = VerifierSpec::FileHash {
            path: "out.txt".into(),
            sha256: digest,
        };
        assert_eq!(evaluate(&spec, dir.path()).await, VerifierVerdict::Passed);
    }

    #[tokio::test]
    async fn file_hash_fails_on_mismatched_digest() {
        let dir = tempdir();
        std::fs::write(dir.path().join("out.txt"), "hello").unwrap();
        let spec = VerifierSpec::FileHash {
            path: "out.txt".into(),
            sha256: "0".repeat(64),
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Failed { .. }
        ));
    }

    // ── JsonPredicate ────────────────────────────────────────────────────

    #[tokio::test]
    async fn json_predicate_eq_passes() {
        let dir = tempdir();
        std::fs::write(dir.path().join("r.json"), r#"{"status":"ok"}"#).unwrap();
        let spec = VerifierSpec::JsonPredicate {
            path: "r.json".into(),
            pointer: "/status".into(),
            op: JsonCmpOp::Eq,
            value: serde_json::json!("ok"),
        };
        assert_eq!(evaluate(&spec, dir.path()).await, VerifierVerdict::Passed);
    }

    #[tokio::test]
    async fn json_predicate_eq_fails_on_mismatch() {
        let dir = tempdir();
        std::fs::write(dir.path().join("r.json"), r#"{"status":"failed"}"#).unwrap();
        let spec = VerifierSpec::JsonPredicate {
            path: "r.json".into(),
            pointer: "/status".into(),
            op: JsonCmpOp::Eq,
            value: serde_json::json!("ok"),
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn json_predicate_missing_pointer_fails() {
        let dir = tempdir();
        std::fs::write(dir.path().join("r.json"), r#"{"status":"ok"}"#).unwrap();
        let spec = VerifierSpec::JsonPredicate {
            path: "r.json".into(),
            pointer: "/nope".into(),
            op: JsonCmpOp::Eq,
            value: serde_json::json!("ok"),
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn json_predicate_malformed_json_is_unknown() {
        let dir = tempdir();
        std::fs::write(dir.path().join("r.json"), "not json").unwrap();
        let spec = VerifierSpec::JsonPredicate {
            path: "r.json".into(),
            pointer: "".into(),
            op: JsonCmpOp::Eq,
            value: serde_json::json!(null),
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Unknown { .. }
        ));
    }

    #[tokio::test]
    async fn json_predicate_contains_passes_on_substring() {
        let dir = tempdir();
        std::fs::write(
            dir.path().join("r.json"),
            r#"{"log":"build succeeded at 12:00"}"#,
        )
        .unwrap();
        let spec = VerifierSpec::JsonPredicate {
            path: "r.json".into(),
            pointer: "/log".into(),
            op: JsonCmpOp::Contains,
            value: serde_json::json!("succeeded"),
        };
        assert_eq!(evaluate(&spec, dir.path()).await, VerifierVerdict::Passed);
    }

    // ── AskHuman ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn ask_human_always_needs_human_never_passed() {
        let dir = tempdir();
        let spec = VerifierSpec::AskHuman {
            question: "Did it work?".into(),
            options: vec!["Yes".into(), "No".into()],
        };
        let v = evaluate(&spec, dir.path()).await;
        match v {
            VerifierVerdict::NeedsHuman { question, options } => {
                assert_eq!(question, "Did it work?");
                assert_eq!(options, vec!["Yes".to_string(), "No".to_string()]);
            }
            other => {
                panic!("AskHuman must never resolve to anything but NeedsHuman, got {other:?}")
            }
        }
    }

    // ── All / Any ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn all_passes_only_when_every_sub_spec_passes() {
        let dir = tempdir();
        std::fs::write(dir.path().join("a"), "x").unwrap();
        let all_pass = VerifierSpec::All {
            specs: vec![
                VerifierSpec::FileExists {
                    path: "a".into(),
                    min_bytes: None,
                },
                VerifierSpec::FileExists {
                    path: "a".into(),
                    min_bytes: None,
                },
            ],
        };
        assert_eq!(
            evaluate(&all_pass, dir.path()).await,
            VerifierVerdict::Passed
        );

        let one_fails = VerifierSpec::All {
            specs: vec![
                VerifierSpec::FileExists {
                    path: "a".into(),
                    min_bytes: None,
                },
                VerifierSpec::FileExists {
                    path: "missing".into(),
                    min_bytes: None,
                },
            ],
        };
        assert!(matches!(
            evaluate(&one_fails, dir.path()).await,
            VerifierVerdict::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn any_passes_when_at_least_one_sub_spec_passes() {
        let dir = tempdir();
        std::fs::write(dir.path().join("a"), "x").unwrap();
        let spec = VerifierSpec::Any {
            specs: vec![
                VerifierSpec::FileExists {
                    path: "missing".into(),
                    min_bytes: None,
                },
                VerifierSpec::FileExists {
                    path: "a".into(),
                    min_bytes: None,
                },
            ],
        };
        assert_eq!(evaluate(&spec, dir.path()).await, VerifierVerdict::Passed);
    }

    #[tokio::test]
    async fn any_fails_when_every_sub_spec_fails() {
        let dir = tempdir();
        let spec = VerifierSpec::Any {
            specs: vec![
                VerifierSpec::FileExists {
                    path: "missing1".into(),
                    min_bytes: None,
                },
                VerifierSpec::FileExists {
                    path: "missing2".into(),
                    min_bytes: None,
                },
            ],
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Failed { .. }
        ));
    }

    // ── Unsupported ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn unsupported_is_always_unknown_never_passed() {
        let dir = tempdir();
        let spec = VerifierSpec::Unsupported;
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Unknown { .. }
        ));
    }

    // ── HttpPredicate ────────────────────────────────────────────────────
    // A minimal hand-rolled HTTP/1.1 server, not a real network dependency:
    // the whole point of this test is that HttpPredicate actually makes a
    // request and inspects the response, not just parses a spec.

    async fn serve_once(status_line: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn http_predicate_passes_on_matching_status_and_body() {
        let dir = tempdir();
        let url = serve_once("HTTP/1.1 200 OK", "build succeeded").await;
        let spec = VerifierSpec::HttpPredicate {
            url,
            expect_status: Some(200),
            body_contains: Some("succeeded".into()),
        };
        assert_eq!(evaluate(&spec, dir.path()).await, VerifierVerdict::Passed);
    }

    #[tokio::test]
    async fn http_predicate_fails_on_wrong_status() {
        let dir = tempdir();
        let url = serve_once("HTTP/1.1 500 Internal Server Error", "oops").await;
        let spec = VerifierSpec::HttpPredicate {
            url,
            expect_status: Some(200),
            body_contains: None,
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn http_predicate_fails_on_missing_body_substring() {
        let dir = tempdir();
        let url = serve_once("HTTP/1.1 200 OK", "build failed").await;
        let spec = VerifierSpec::HttpPredicate {
            url,
            expect_status: Some(200),
            body_contains: Some("succeeded".into()),
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn http_predicate_unreachable_url_is_unknown_not_failed() {
        let dir = tempdir();
        // Port 0 immediately refused; nothing is listening there.
        let spec = VerifierSpec::HttpPredicate {
            url: "http://127.0.0.1:1/".into(),
            expect_status: Some(200),
            body_contains: None,
        };
        assert!(matches!(
            evaluate(&spec, dir.path()).await,
            VerifierVerdict::Unknown { .. }
        ));
    }

    // ── VerifyExecutor ──────────────────────────────────────────────────────

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum TS {
        Top,
        Idle,
        Done,
    }
    struct OneShotMachine(sven_hsm::MachineId);
    impl OneShotMachine {
        fn new() -> Self {
            Self(sven_hsm::MachineId::new())
        }
    }
    impl sven_hsm::Machine for OneShotMachine {
        type State = TS;
        fn id(&self) -> sven_hsm::MachineId {
            self.0
        }
        fn top(&self) -> TS {
            TS::Top
        }
        fn initial(&self) -> TS {
            TS::Idle
        }
        fn superstate(&self, _s: TS) -> TS {
            TS::Top
        }
        fn is_terminal(&self, s: TS) -> bool {
            s == TS::Done
        }
        fn dispatch_state(
            &mut self,
            s: TS,
            e: &sven_hsm::Event,
            ctx: &mut sven_hsm::Context,
        ) -> sven_hsm::Reaction<TS> {
            match s {
                TS::Top => sven_hsm::Reaction::Handled(vec![]),
                TS::Idle => {
                    if e.is_lifecycle() {
                        return sven_hsm::Reaction::Handled(vec![]);
                    }
                    if let sven_hsm::Event::VerificationComplete { verdict } = e {
                        ctx.set_fact("verdict", serde_json::to_value(verdict).unwrap());
                    }
                    sven_hsm::Reaction::Transition {
                        target: TS::Done,
                        effects: vec![],
                        rationale: "got event".into(),
                    }
                }
                TS::Done => sven_hsm::Reaction::Handled(vec![]),
            }
        }
    }

    struct NoOpExec;
    #[async_trait::async_trait]
    impl sven_kernel::EffectExecutor for NoOpExec {
        async fn execute(
            &mut self,
            _: sven_hsm::Effect,
            _: &sven_kernel::EventSink,
            _: &sven_hsm::ObservationSink,
        ) {
        }
    }

    async fn run_verify_effect(exec: &mut VerifyExecutor, spec: VerifierSpec) -> VerifierVerdict {
        let rt = sven_kernel::Runtime::spawn(
            sven_hsm::Hsm::new(OneShotMachine::new()),
            sven_hsm::Context::new(),
            sven_hsm::PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        exec.execute(
            sven_hsm::Effect::Verify { spec },
            &sink,
            &sven_hsm::ObservationSink::default(),
        )
        .await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        let verdict = report
            .ctx
            .fact("verdict")
            .cloned()
            .expect("verdict fact set");
        serde_json::from_value(verdict).unwrap()
    }

    #[tokio::test]
    async fn verify_executor_posts_verification_complete() {
        let dir = tempdir();
        std::fs::write(dir.path().join("out.txt"), "hi").unwrap();
        let mut exec = VerifyExecutor::new(dir.path());
        let spec = VerifierSpec::FileExists {
            path: "out.txt".into(),
            min_bytes: None,
        };
        assert_eq!(
            run_verify_effect(&mut exec, spec).await,
            VerifierVerdict::Passed
        );
    }

    #[tokio::test]
    async fn verify_executor_jails_to_its_own_root_not_a_spec_supplied_one() {
        let dir = tempdir();
        let mut exec = VerifyExecutor::new(dir.path());
        let spec = VerifierSpec::FileExists {
            path: "/etc/passwd".into(),
            min_bytes: None,
        };
        assert!(matches!(
            run_verify_effect(&mut exec, spec).await,
            VerifierVerdict::Unknown { .. }
        ));
    }

    #[tokio::test]
    async fn verify_executor_ignores_non_verify_effects() {
        let dir = tempdir();
        let mut exec = VerifyExecutor::new(dir.path());
        let rt = sven_kernel::Runtime::spawn(
            sven_hsm::Hsm::new(OneShotMachine::new()),
            sven_hsm::Context::new(),
            sven_hsm::PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        exec.execute(
            sven_hsm::Effect::PersistAudit,
            &sink,
            &sven_hsm::ObservationSink::default(),
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !rt.status().done,
            "a non-Verify effect must not post anything"
        );
        rt.abort();
    }
}
