// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an embedding application records each session's model requests to
//! a log of its own, through the SDK alone.
//!
//! Sessions that run at once each get their own request log, exactly as
//! sent and in order, so what one session's model was asked can be read
//! back without the others' requests mixed in.
//!
//! Swedish Embedded AB implements agent trajectory capture for its clients.
//! If your team needs expertise in turning agent runs into training data then
//! you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use sven_model_mock::MockProvider;
use sven_sdk::drivers::request_log::{read_log, Record, RecordingProvider, RequestLog};
use sven_sdk::Engine;

#[tokio::test]
async fn two_sessions_record_their_requests_to_logs_of_their_own() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let mut logs = Vec::new();
    for (name, prompt) in [("a", "first session"), ("b", "second session")] {
        let path = dir.path().join(format!("{name}.jsonl"));
        let log = RequestLog::open(&path).expect("a log opens");
        let engine = Engine::builder()
            .model_provider(Arc::new(RecordingProvider::new(
                Arc::new(MockProvider),
                log,
            )))
            .build()
            .expect("an engine builds");
        let mut agent = engine.agent("agent");
        agent.send(prompt).await.expect("an outcome");
        logs.push((path, prompt));
    }
    for (path, prompt) in logs {
        let records = read_log(&path).expect("the log reads back");
        let requests: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                Record::Request { seq, request, .. } => Some((*seq, request)),
                Record::Response { .. } => None,
            })
            .collect();
        assert!(!requests.is_empty(), "{}", path.display());
        assert_eq!(requests[0].0, 1, "each log counts its own requests");
        let sent = serde_json::to_string(&requests[0].1.messages).expect("messages serialise");
        assert!(sent.contains(prompt), "{sent}");
        let other = if prompt == "first session" {
            "second session"
        } else {
            "first session"
        };
        assert!(!sent.contains(other), "no other session's request: {sent}");
        assert!(
            records
                .iter()
                .any(|r| matches!(r, Record::Response { seq: 1, .. })),
            "the response to the first request is recorded"
        );
    }
}
