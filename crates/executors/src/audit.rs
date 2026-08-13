// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Hash-chained audit log effect executor.
//!
//! Handles [`Effect::PersistAudit`] by appending audit entries to a durable
//! JSONL file on disk, using the hash-chain primitives in [`sven_chain`]
//! (line format, integrity guarantees and their limits, concurrency/crash
//! safety, and legacy-file rotation are all documented there — this module
//! only adds the audit-specific entry shape and the `EffectExecutor` glue).
//!
//! [`Effect::PersistAudit`] carries no payload: the kernel's own
//! [`sven_hsm::Context::audit`] field is the ground-truth audit trail. The
//! kernel mirrors it (and the per-tool-call trail) into an
//! [`AuditTrailHandle`] around every dispatch and executes `PersistAudit`
//! itself after each dispatch's effects have run, so an executor built with
//! [`AuditExecutor::with_trail`] flushes the **full** [`AuditRecord`]s
//! (including principal attribution) and `ToolAuditRecord`s that accumulated
//! since the previous flush. Without a trail ([`AuditExecutor::new`]) each
//! `PersistAudit` writes a chained `checkpoint` marker entry, preserving the
//! historical behavior.
//!
//! [`AuditRecord`]: sven_hsm::AuditRecord

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Utc;
pub use sven_chain::{append_chain, read_chain, verify_chain, ChainError, ChainedLine, GENESIS_HASH};
use sven_hsm::{AuditTrailHandle, Effect, ObservationSink};
use sven_kernel::{EffectExecutor, EventSink};

/// Executes [`Effect::PersistAudit`] by appending hash-chained entries to an
/// append-only JSONL log (see the module docs for the line format and for
/// the concurrency/crash-safety behavior).
pub struct AuditExecutor {
    log_path: PathBuf,
    /// Shared mirror of the kernel's audit trail; `None` = checkpoint-marker
    /// mode (historical behavior of [`AuditExecutor::new`]).
    trail: Option<AuditTrailHandle>,
    /// How many dispatch records have already been flushed to disk.
    persisted_records: usize,
    /// How many tool records have already been flushed to disk.
    persisted_tool_records: usize,
}

impl AuditExecutor {
    /// Creates an executor that writes to `log_path`.
    ///
    /// The file is created if it does not exist and opened in append mode.
    /// Without an [`AuditTrailHandle`] each `PersistAudit` appends one chained
    /// `checkpoint` marker entry; use [`Self::with_trail`] to persist the full
    /// audit records.
    pub fn new(log_path: impl Into<PathBuf>) -> Self {
        Self {
            log_path: log_path.into(),
            trail: None,
            persisted_records: 0,
            persisted_tool_records: 0,
        }
    }

    /// Creates an executor that flushes the full audit records mirrored in
    /// `trail` (dispatch records first, then tool records) on every
    /// `PersistAudit`, each as its own hash-chained JSONL line.
    pub fn with_trail(log_path: impl Into<PathBuf>, trail: AuditTrailHandle) -> Self {
        Self {
            log_path: log_path.into(),
            trail: Some(trail),
            persisted_records: 0,
            persisted_tool_records: 0,
        }
    }

    /// Collects the entries this flush should append, returning them together
    /// with the post-flush cursor positions.
    fn pending_entries(&self, timestamp: &str) -> (Vec<serde_json::Value>, usize, usize) {
        let Some(trail) = &self.trail else {
            let marker = serde_json::json!({
                "kind": "checkpoint",
                "timestamp": timestamp,
            });
            return (
                vec![marker],
                self.persisted_records,
                self.persisted_tool_records,
            );
        };

        // Fetch only the not-yet-persisted suffixes; the trail is
        // append-only, so the cursors stay valid across flushes.
        let records = trail.records_from(self.persisted_records);
        let tool_records = trail.tool_records_from(self.persisted_tool_records);
        let mut entries = Vec::with_capacity(records.len() + tool_records.len());
        for record in &records {
            entries.push(serde_json::json!({
                "kind": "dispatch",
                "timestamp": timestamp,
                "record": record,
            }));
        }
        for record in &tool_records {
            entries.push(serde_json::json!({
                "kind": "tool",
                "timestamp": timestamp,
                "record": record,
            }));
        }
        (
            entries,
            self.persisted_records + records.len(),
            self.persisted_tool_records + tool_records.len(),
        )
    }
}

#[async_trait]
impl EffectExecutor for AuditExecutor {
    async fn execute(&mut self, effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {
        if !matches!(effect, Effect::PersistAudit) {
            return;
        }

        let timestamp = Utc::now().to_rfc3339();
        let (entries, next_records, next_tool_records) = self.pending_entries(&timestamp);
        if entries.is_empty() {
            tracing::debug!(
                path = %self.log_path.display(),
                "AuditExecutor: no new audit records to persist"
            );
            return;
        }

        let path = self.log_path.clone();
        let written = entries.len();

        // Perform the I/O on a blocking thread so we do not block the tokio
        // executor.
        let result = tokio::task::spawn_blocking(move || append_chain(&path, entries)).await;

        match result {
            Ok(Ok(_)) => {
                // Advance the cursors only after a successful write so failed
                // flushes are retried on the next `PersistAudit`.
                self.persisted_records = next_records;
                self.persisted_tool_records = next_tool_records;
                tracing::debug!(
                    path = %self.log_path.display(),
                    entries = written,
                    "AuditExecutor: audit records persisted"
                );
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
        AuditRecord, AuditTrailHandle, Context, Effect, Event, EventKind, Hsm, MachineId,
        ObservationSink, PermissionPolicy, Principal, Reaction, ToolAuditRecord, ToolCallId,
        ToolCapability,
    };
    use sven_kernel::{EffectExecutor, EventSink, Runtime};

    use super::{verify_chain, AuditExecutor, ChainedLine, GENESIS_HASH};

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

    /// Spawns a throwaway runtime just to obtain a live `EventSink`.
    fn sink_fixture() -> (Runtime<TinyMachine>, EventSink) {
        let rt = Runtime::spawn(
            Hsm::new(TinyMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        (rt, sink)
    }

    async fn flush(exec: &mut AuditExecutor, sink: &EventSink) {
        exec.execute(Effect::PersistAudit, sink, &ObservationSink::default())
            .await;
    }

    fn parse_lines(path: &std::path::Path) -> Vec<ChainedLine> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn persist_audit_without_trail_writes_chained_checkpoint() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let mut exec = AuditExecutor::new(&log_path);
        let (rt, sink) = sink_fixture();

        flush(&mut exec, &sink).await;

        let lines = parse_lines(&log_path);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].prev_hash, GENESIS_HASH);
        assert_eq!(lines[0].entry["kind"].as_str(), Some("checkpoint"));
        assert_eq!(verify_chain(&log_path).unwrap(), 1);
        rt.abort();
    }

    #[tokio::test]
    async fn persist_audit_appends_and_links_multiple_lines() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let mut exec = AuditExecutor::new(&log_path);
        let (rt, sink) = sink_fixture();

        flush(&mut exec, &sink).await;
        flush(&mut exec, &sink).await;

        let lines = parse_lines(&log_path);
        assert_eq!(lines.len(), 2, "expected exactly 2 lines");
        assert_eq!(
            lines[1].prev_hash, lines[0].hash,
            "second line must chain from the first"
        );
        assert_eq!(verify_chain(&log_path).unwrap(), 2);
        rt.abort();
    }

    #[tokio::test]
    async fn non_persist_audit_effect_is_ignored() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let mut exec = AuditExecutor::new(&log_path);
        let (rt, sink) = sink_fixture();

        exec.execute(
            Effect::EmitInternal {
                name: "test".into(),
                payload: serde_json::Value::Null,
            },
            &sink,
            &ObservationSink::default(),
        )
        .await;

        assert!(
            !log_path.exists(),
            "log should not be created for unhandled effects"
        );
        rt.abort();
    }

    #[tokio::test]
    async fn trail_flushes_full_records_with_principal_and_tool_audit() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");

        let trail = AuditTrailHandle::new();
        let mut ctx = Context::new();
        ctx.principal = Some(Principal::new("acme", "alice"));
        ctx.push_audit(AuditRecord::transition(
            "Idle",
            "Working",
            EventKind::UserMessage,
            &[],
            Some("user asked".into()),
        ));
        ctx.push_tool_audit(ToolAuditRecord::started(
            "Working",
            ToolCallId::new(),
            "read_file",
            ToolCapability::ReadFile,
        ));
        trail.sync_from(&ctx);

        let mut exec = AuditExecutor::with_trail(&log_path, trail);
        let (rt, sink) = sink_fixture();
        flush(&mut exec, &sink).await;

        let lines = parse_lines(&log_path);
        assert_eq!(lines.len(), 2, "one dispatch + one tool entry");

        // The full AuditRecord round-trips, including principal attribution.
        assert_eq!(lines[0].entry["kind"].as_str(), Some("dispatch"));
        let record: AuditRecord = serde_json::from_value(lines[0].entry["record"].clone()).unwrap();
        assert_eq!(record.from_state, "Idle");
        assert_eq!(record.to_state, "Working");
        assert_eq!(record.rationale.as_deref(), Some("user asked"));
        assert_eq!(record.tenant_id.as_deref(), Some("acme"));
        assert_eq!(record.actor_id.as_deref(), Some("alice"));

        // The ToolAuditRecord is attributed too — the durable log must not
        // need call_id correlation to know who ran which tool.
        assert_eq!(lines[1].entry["kind"].as_str(), Some("tool"));
        let tool: ToolAuditRecord =
            serde_json::from_value(lines[1].entry["record"].clone()).unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.tenant_id.as_deref(), Some("acme"));
        assert_eq!(tool.actor_id.as_deref(), Some("alice"));

        assert_eq!(verify_chain(&log_path).unwrap(), 2);
        rt.abort();
    }

    #[tokio::test]
    async fn trail_flushes_are_incremental() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");

        let trail = AuditTrailHandle::new();
        let mut ctx = Context::new();
        ctx.push_audit(AuditRecord::ignored("Idle", EventKind::UserMessage));
        trail.sync_from(&ctx);

        let mut exec = AuditExecutor::with_trail(&log_path, trail.clone());
        let (rt, sink) = sink_fixture();

        flush(&mut exec, &sink).await;
        assert_eq!(parse_lines(&log_path).len(), 1);

        // Nothing new: flush writes nothing.
        flush(&mut exec, &sink).await;
        assert_eq!(parse_lines(&log_path).len(), 1, "no duplicate records");

        // One more record: only the delta is appended.
        ctx.push_audit(AuditRecord::ignored("Idle", EventKind::Timeout));
        trail.sync_from(&ctx);
        flush(&mut exec, &sink).await;

        let lines = parse_lines(&log_path);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].prev_hash, lines[0].hash);
        assert_eq!(verify_chain(&log_path).unwrap(), 2);
        rt.abort();
    }

    #[tokio::test]
    async fn new_executor_resumes_existing_chain() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let (rt, sink) = sink_fixture();

        let mut first = AuditExecutor::new(&log_path);
        flush(&mut first, &sink).await;

        // A fresh executor instance (e.g. after a restart) must continue the
        // chain from the last line on disk, not restart from genesis.
        let mut second = AuditExecutor::new(&log_path);
        flush(&mut second, &sink).await;

        let lines = parse_lines(&log_path);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].prev_hash, lines[0].hash);
        assert_eq!(verify_chain(&log_path).unwrap(), 2);
        rt.abort();
    }

    #[tokio::test]
    async fn interleaved_executors_never_fork_the_chain() {
        // Regression: two executors appending to the same file used to trust
        // their in-memory prev_hash, so A/B/A interleaving broke the chain at
        // line 3 forever. Every flush must chain from the on-disk tip.
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let (rt, sink) = sink_fixture();

        let mut a = AuditExecutor::new(&log_path);
        let mut b = AuditExecutor::new(&log_path);

        flush(&mut a, &sink).await;
        flush(&mut b, &sink).await;
        flush(&mut a, &sink).await;

        assert_eq!(
            verify_chain(&log_path).unwrap(),
            3,
            "interleaved writers must extend one linear chain"
        );
        rt.abort();
    }

    #[tokio::test]
    async fn legacy_pre_chain_file_is_rotated_so_new_records_verify() {
        // A pre-chain first line used to make verify_chain fail at line 1
        // forever, silently disabling verification for all NEW records too.
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        std::fs::write(
            &log_path,
            "{\"kind\":\"PersistAudit\",\"timestamp\":\"2025-01-01T00:00:00Z\"}\n",
        )
        .unwrap();
        assert!(
            verify_chain(&log_path).is_err(),
            "legacy file cannot verify"
        );

        let mut exec = AuditExecutor::new(&log_path);
        let (rt, sink) = sink_fixture();
        flush(&mut exec, &sink).await;

        assert_eq!(
            verify_chain(&log_path).unwrap(),
            1,
            "new records must verify after legacy rotation"
        );
        let legacy_rotated = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("audit.jsonl.legacy-")
            });
        assert!(legacy_rotated, "legacy file must be preserved, not deleted");
        rt.abort();
    }

    #[tokio::test]
    async fn torn_trailing_line_is_repaired_on_next_flush() {
        // A crash mid-write leaves a partial (non-newline-terminated) last
        // line. The next flush must truncate it and keep the chain verifiable
        // instead of forking from GENESIS.
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let (rt, sink) = sink_fixture();

        let mut exec = AuditExecutor::new(&log_path);
        flush(&mut exec, &sink).await;
        assert_eq!(verify_chain(&log_path).unwrap(), 1);

        // Simulate a torn write.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"{\"prev_hash\":\"deadbeef\",\"ha").unwrap();
        }
        assert!(verify_chain(&log_path).is_err(), "torn tail breaks verify");

        flush(&mut exec, &sink).await;
        assert_eq!(
            verify_chain(&log_path).unwrap(),
            2,
            "flush must truncate the torn tail and continue the chain"
        );
        rt.abort();
    }

    #[tokio::test]
    async fn erased_runtime_session_writes_verifiable_audit_log() {
        use crate::CompositeExecutorBuilder;
        use sven_kernel::ErasedRuntime;

        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("audit.jsonl");

        let trail = AuditTrailHandle::new();
        let executor = CompositeExecutorBuilder::default()
            .with_audit_trail(&log_path, trail.clone())
            .build();

        let mut ctx = Context::new();
        ctx.principal = Some(Principal::new("acme", "alice"));
        let rt = ErasedRuntime::spawn_with_audit_trail(
            Box::new(Hsm::new(TinyMachine::new())),
            ctx,
            PermissionPolicy::builder().build(),
            executor,
            16,
            None,
            trail,
        );

        rt.post(Event::UserMessage {
            text: "hello".into(),
        })
        .await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        assert_eq!(report.state_label, "Done");

        let verified = verify_chain(&log_path).unwrap();
        assert!(
            verified > 0,
            "a real session must leave a non-empty audit log"
        );
        let lines = parse_lines(&log_path);
        let dispatch_line = lines
            .iter()
            .find(|l| l.entry["kind"].as_str() == Some("dispatch"))
            .expect("at least one dispatch record persisted");
        assert_eq!(
            dispatch_line.entry["record"]["tenant_id"].as_str(),
            Some("acme"),
            "persisted records carry principal attribution"
        );
    }

}
