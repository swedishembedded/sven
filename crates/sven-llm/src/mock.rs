//! Deterministic mock adapter for testing.
//!
//! [`MockLlmAdapter`] accepts a pre-scripted queue of [`sven_hsm::Event`]s and
//! dequeues one per [`LlmAdapter::invoke`] call.  This makes LLM steps fully
//! deterministic in tests without any network access.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use sven_hsm::{Event, ObservationSink};

use crate::adapter::LlmAdapter;
use crate::error::LlmError;
use crate::request::LlmRequest;

/// A scripted mock that returns pre-programmed events in order.
///
/// # Example
///
/// ```
/// use sven_llm::mock::MockLlmAdapter;
/// use sven_hsm::Event;
/// use serde_json::json;
///
/// let adapter = MockLlmAdapter::new(vec![
///     Event::LlmProposedAssessment { assessment: json!({"intent": "bugfix", "confidence": 0.9}) },
/// ]);
/// ```
pub struct MockLlmAdapter {
    queue: Mutex<VecDeque<Event>>,
    /// The last request seen by this adapter (useful in tests).
    pub last_request: Mutex<Option<LlmRequest>>,
}

impl MockLlmAdapter {
    /// Build a mock from an ordered list of events.  The first event in the
    /// list is returned by the first `invoke` call, the second by the second,
    /// and so on.
    pub fn new(events: Vec<Event>) -> Self {
        Self {
            queue: Mutex::new(VecDeque::from(events)),
            last_request: Mutex::new(None),
        }
    }

    /// Convenience: a mock that always returns `Event::LlmFailed` (no events
    /// in the queue → fallback).
    pub fn empty() -> Self {
        Self::new(vec![])
    }

    /// How many events are still queued.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.queue.lock().expect("mock lock poisoned").len()
    }
}

#[async_trait]
impl LlmAdapter for MockLlmAdapter {
    async fn invoke(&self, req: LlmRequest, _obs: Option<&ObservationSink>) -> Result<Event, LlmError> {
        // Record the request for inspection.
        *self.last_request.lock().expect("mock lock poisoned") = Some(req.clone());

        let event = self
            .queue
            .lock()
            .expect("mock lock poisoned")
            .pop_front()
            .unwrap_or(Event::LlmFailed {
                error: format!(
                    "MockLlmAdapter: no more scripted events (request was {})",
                    req.kind_name()
                ),
            });
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sven_hsm::EventKind;

    fn make_assessment(intent: &str) -> Event {
        Event::LlmProposedAssessment {
            assessment: json!({"intent": intent, "confidence": 0.9}),
        }
    }

    #[tokio::test]
    async fn dequeues_events_in_order() {
        let adapter =
            MockLlmAdapter::new(vec![make_assessment("bugfix"), make_assessment("feature")]);
        let req = LlmRequest::ExtractIntent {
            text: "any".into(),
            allowed_intents: vec!["bugfix".into(), "feature".into()],
        };

        let e1 = adapter.invoke(req.clone(), None).await.unwrap();
        let e2 = adapter.invoke(req.clone(), None).await.unwrap();

        assert_eq!(e1.kind(), EventKind::LlmProposedAssessment);
        assert_eq!(e2.kind(), EventKind::LlmProposedAssessment);
        // Third invoke returns LlmFailed
        let e3 = adapter.invoke(req, None).await.unwrap();
        assert_eq!(e3.kind(), EventKind::LlmFailed);
    }

    #[tokio::test]
    async fn records_last_request() {
        let adapter = MockLlmAdapter::new(vec![make_assessment("bugfix")]);
        let req = LlmRequest::ExtractConstraints {
            known_context: json!({"goal": "test"}),
        };
        let _ = adapter.invoke(req, None).await.unwrap();
        let last = adapter.last_request.lock().unwrap();
        assert!(matches!(
            last.as_ref(),
            Some(LlmRequest::ExtractConstraints { .. })
        ));
    }

    #[tokio::test]
    async fn empty_adapter_returns_llm_failed() {
        let adapter = MockLlmAdapter::empty();
        let req = LlmRequest::ExtractIntent {
            text: "x".into(),
            allowed_intents: vec![],
        };
        let event = adapter.invoke(req, None).await.unwrap();
        assert_eq!(event.kind(), EventKind::LlmFailed);
    }
}
