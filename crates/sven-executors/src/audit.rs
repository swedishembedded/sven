//! Audit log effect executor.
//!
//! Handles [`Effect::PersistAudit`] by appending a JSON-lines marker to a
//! durable audit log file on disk.
//!
//! # Design note
//!
//! [`Effect::PersistAudit`] carries no payload: the kernel's own
//! [`sven_hsm::Context::audit`] field is the ground-truth audit trail, kept
//! in memory by the runtime and available via
//! [`sven_hsm::Runtime::audit_snapshot`].  This executor's role is to flush a
//! "persistence triggered" marker to disk so that an out-of-process reader
//! can know the runtime reached a stable audit checkpoint.  A richer
//! implementation can combine this executor with an
//! `Arc<Mutex<Vec<AuditRecord>>>` shared with the runtime.

use std::io::Write;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Utc;
use sven_hsm::{Effect, EffectExecutor, EventSink, ObservationSink};

/// Executes [`Effect::PersistAudit`] by appending to an append-only JSONL log.
pub struct AuditExecutor {
    log_path: PathBuf,
}

impl AuditExecutor {
    /// Creates an executor that writes to `log_path`.
    ///
    /// The file is created if it does not exist and opened in append mode.
    pub fn new(log_path: impl Into<PathBuf>) -> Self {
        Self {
            log_path: log_path.into(),
        }
    }
}

#[async_trait]
impl EffectExecutor for AuditExecutor {
    async fn execute(&mut self, effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {
        if !matches!(effect, Effect::PersistAudit) {
            return;
        }

        let path = self.log_path.clone();
        let timestamp = Utc::now().to_rfc3339();

        // Perform the I/O on a blocking thread so we do not block the tokio
        // executor.
        let result = tokio::task::spawn_blocking(move || {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            let record = serde_json::json!({
                "kind": "PersistAudit",
                "timestamp": timestamp,
            });
            writeln!(file, "{}", record)?;
            file.flush()?;
            Ok::<_, std::io::Error>(())
        })
        .await;

        match result {
            Ok(Ok(())) => {
                tracing::debug!(path = %self.log_path.display(), "AuditExecutor: audit record persisted");
            }
            Ok(Err(e)) => {
                tracing::warn!(path = %self.log_path.display(), error = %e, "AuditExecutor: failed to write audit log");
            }
            Err(e) => {
                tracing::warn!(error = %e, "AuditExecutor: spawn_blocking panicked");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sven_hsm::{
        Context, Effect, EffectExecutor, Event, EventSink, Hsm, MachineId, ObservationSink,
        PermissionPolicy, Reaction, Runtime,
    };

    use super::AuditExecutor;

    struct NoOpExec;
    #[async_trait::async_trait]
    impl EffectExecutor for NoOpExec {
        async fn execute(&mut self, _: Effect, _: &EventSink, _: &ObservationSink) {}
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum TS {
        Top,
        Idle,
        Done,
    }
    struct TinyMachine(MachineId);
    impl TinyMachine {
        fn new() -> Self {
            Self(MachineId::new())
        }
    }
    impl sven_hsm::Machine for TinyMachine {
        type State = TS;
        fn id(&self) -> MachineId {
            self.0
        }
        fn top(&self) -> TS {
            TS::Top
        }
        fn initial(&self) -> TS {
            TS::Idle
        }
        fn superstate(&self, s: TS) -> TS {
            match s {
                TS::Top => TS::Top,
                _ => TS::Top,
            }
        }
        fn is_terminal(&self, s: TS) -> bool {
            s == TS::Done
        }
        fn dispatch_state(&mut self, s: TS, e: &Event, _ctx: &mut Context) -> Reaction<TS> {
            match s {
                TS::Top | TS::Done => Reaction::Handled(vec![]),
                TS::Idle => {
                    if e.is_lifecycle() {
                        Reaction::Handled(vec![])
                    } else {
                        Reaction::Transition {
                            target: TS::Done,
                            effects: vec![],
                            rationale: "done".into(),
                        }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn persist_audit_writes_jsonl_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let mut exec = AuditExecutor::new(&log_path);

        let rt = Runtime::spawn(
            Hsm::new(TinyMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();

        exec.execute(
            Effect::PersistAudit,
            &sink,
            &sven_hsm::ObservationSink::default(),
        )
        .await;

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            content.contains("PersistAudit"),
            "expected audit record in log"
        );

        // Verify it is valid JSON on the first line.
        let first_line = content.lines().next().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(first_line).unwrap();
        assert_eq!(parsed["kind"].as_str(), Some("PersistAudit"));

        rt.abort();
    }

    #[tokio::test]
    async fn persist_audit_appends_multiple_lines() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let mut exec = AuditExecutor::new(&log_path);

        let rt = Runtime::spawn(
            Hsm::new(TinyMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();

        exec.execute(
            Effect::PersistAudit,
            &sink,
            &sven_hsm::ObservationSink::default(),
        )
        .await;
        exec.execute(
            Effect::PersistAudit,
            &sink,
            &sven_hsm::ObservationSink::default(),
        )
        .await;

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(content.lines().count(), 2, "expected exactly 2 lines");
        rt.abort();
    }

    #[tokio::test]
    async fn non_persist_audit_effect_is_ignored() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let mut exec = AuditExecutor::new(&log_path);

        let rt = Runtime::spawn(
            Hsm::new(TinyMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();

        // Non-PersistAudit effects should be silently ignored.
        exec.execute(
            Effect::EmitInternal {
                name: "test".into(),
                payload: serde_json::Value::Null,
            },
            &sink,
            &sven_hsm::ObservationSink::default(),
        )
        .await;

        assert!(
            !log_path.exists(),
            "log should not be created for unhandled effects"
        );
        rt.abort();
    }
}
