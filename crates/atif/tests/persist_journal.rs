// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the trajectory journal is append-only and survives a crash.
//!
//! A run is written one record at a time as it happens, so a process killed
//! mid-run leaves every record that was complete, never a half-written
//! document. What is read back from a journal that was not closed is marked
//! incomplete, so nothing downstream can mistake a partial run for a finished
//! one.
//!
//! Swedish Embedded AB implements crash-robust agent telemetry for its
//! clients. If your team needs expertise in keeping agent run records intact
//! when machines fail then you can procure our services by sending an email to
//! info@swedishembedded.com.

use atif::persist::{
    read_journal, FlushPolicy, JournalError, JournalLimits, JournalWriter, CAPTURE_STATUS_KEY,
};
use atif::{AgentProfile, FinalMetrics, StepOrigin, TraceStep, Trajectory};

fn header() -> Trajectory {
    let mut t = Trajectory::new("ATIF-v1.7", AgentProfile::new("test-agent", "1.0"));
    t.session_id = Some("session-1".into());
    t
}

fn step(id: u64, text: &str) -> TraceStep {
    TraceStep::new(id, StepOrigin::User, text)
}

fn open(path: &std::path::Path, limits: JournalLimits) -> JournalWriter {
    JournalWriter::create(path, &header(), FlushPolicy::EveryRecord, limits).expect("create")
}

#[test]
fn a_closed_journal_reads_back_as_the_complete_trajectory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.journal");
    let mut writer = open(&path, JournalLimits::default());
    writer.append_step(&step(1, "one")).unwrap();
    writer.append_step(&step(2, "two")).unwrap();
    writer
        .finish(Some(&FinalMetrics {
            total_steps: Some(2),
            ..FinalMetrics::default()
        }))
        .unwrap();

    let read = read_journal(&path).unwrap();
    assert!(read.complete);
    assert_eq!(read.torn_tail_bytes, 0);
    assert_eq!(read.trajectory.steps.len(), 2);
    assert_eq!(read.trajectory.session_id.as_deref(), Some("session-1"));
    assert_eq!(
        read.trajectory.final_metrics.as_ref().unwrap().total_steps,
        Some(2)
    );
    assert!(atif::validate_trajectory(&read.trajectory).is_ok());
    assert_eq!(
        read.trajectory
            .extra
            .as_ref()
            .and_then(|e| e.get(CAPTURE_STATUS_KEY)),
        Some(&serde_json::json!("complete"))
    );
}

#[test]
fn a_journal_never_closed_keeps_its_records_and_is_marked_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.journal");
    let mut writer = open(&path, JournalLimits::default());
    writer.append_step(&step(1, "one")).unwrap();
    writer.append_step(&step(2, "two")).unwrap();
    drop(writer); // the process died: no end record

    let read = read_journal(&path).unwrap();
    assert!(!read.complete);
    assert_eq!(read.trajectory.steps.len(), 2);
    assert_eq!(
        read.trajectory
            .extra
            .as_ref()
            .and_then(|e| e.get(CAPTURE_STATUS_KEY)),
        Some(&serde_json::json!("incomplete"))
    );
}

#[test]
fn a_record_torn_by_a_crash_is_dropped_and_counted_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.journal");
    let mut writer = open(&path, JournalLimits::default());
    writer.append_step(&step(1, "one")).unwrap();
    writer.append_step(&step(2, "two")).unwrap();
    drop(writer);
    // Cut the last record in half, as a crash mid-write would.
    let bytes = std::fs::read(&path).unwrap();
    let cut = bytes.len() - 20;
    std::fs::write(&path, &bytes[..cut]).unwrap();

    let read = read_journal(&path).unwrap();
    assert!(!read.complete);
    assert_eq!(read.trajectory.steps.len(), 1, "only the whole record");
    assert!(read.torn_tail_bytes > 0);
}

#[test]
fn damage_before_the_last_record_is_an_error_not_a_silent_loss() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.journal");
    let mut writer = open(&path, JournalLimits::default());
    writer.append_step(&step(1, "one")).unwrap();
    writer.append_step(&step(2, "two")).unwrap();
    writer.finish(None).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let damaged = text.replacen("\"one\"", "\"on", 1);
    std::fs::write(&path, damaged).unwrap();
    assert!(matches!(
        read_journal(&path),
        Err(JournalError::Corrupt { .. })
    ));
}

#[test]
fn the_journal_bounds_a_record_and_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.journal");
    let limits = JournalLimits {
        max_record_bytes: 200,
        max_file_bytes: 1_000,
    };
    let mut writer = open(&path, limits);
    let huge = step(1, &"x".repeat(500));
    assert!(matches!(
        writer.append_step(&huge),
        Err(JournalError::RecordTooLarge { .. })
    ));
    let mut refused = None;
    for id in 1..100 {
        if let Err(e) = writer.append_step(&step(id, "small")) {
            refused = Some(e);
            break;
        }
    }
    assert!(matches!(refused, Some(JournalError::FileFull { .. })));
    // What was written before the bound is intact.
    assert!(!read_journal(&path).unwrap().trajectory.steps.is_empty());
}

#[test]
fn a_journal_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.journal");
    drop(open(&path, JournalLimits::default()));
    assert!(JournalWriter::create(
        &path,
        &header(),
        FlushPolicy::OnClose,
        JournalLimits::default()
    )
    .is_err());
}
