// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes whose tools report what they
// read, so that every answer can be traced to its sources, for its clients. If
// your team needs expertise in auditable retrieval you can procure our
// services by sending an email to info@swedishembedded.com.

//! How a tool tells the trajectory what it retrieved.
//!
//! A tool that reads from memory knows which units it returned and which of
//! them it put into the model's context; the executor that ran it knows which
//! call it was. The executor runs the tool inside [`scope`], the tool calls
//! [`record_memory_retrieval`], and the executor files what was recorded under
//! the call's id. A tool run outside an executor (a unit test of the tool
//! itself) records nothing and is told so.

use std::future::Future;
use std::sync::{Arc, Mutex};

use atif::MemoryRetrieval;

tokio::task_local! {
    static RECORDED: Arc<Mutex<Vec<MemoryRetrieval>>>;
}

/// What the executor collects after a tool call.
#[derive(Debug, Clone, Default)]
pub struct CallRecorder(Arc<Mutex<Vec<MemoryRetrieval>>>);

impl CallRecorder {
    /// A recorder with nothing recorded.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything recorded so far, in the order it was recorded.
    #[must_use]
    pub fn memory_retrievals(&self) -> Vec<MemoryRetrieval> {
        self.0.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

/// Runs `future` with `recorder` receiving what the tool records.
///
/// The recording follows the task: a tool that hands the work to a task of its
/// own must record before it does.
pub async fn scope<F: Future>(recorder: CallRecorder, future: F) -> F::Output {
    RECORDED.scope(recorder.0, future).await
}

/// Records that the running tool call retrieved `retrieval`.
///
/// `retrieval.call_id` is overwritten by the executor with the id of the call
/// that made the retrieval, so a tool leaves it empty. Returns `false`, and
/// records nothing, when the tool is not running under an executor.
pub fn record_memory_retrieval(retrieval: MemoryRetrieval) -> bool {
    RECORDED
        .try_with(|recorded| {
            if let Ok(mut recorded) = recorded.lock() {
                recorded.push(retrieval);
            }
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retrieval(query: &str) -> MemoryRetrieval {
        MemoryRetrieval {
            call_id: String::new(),
            query: query.to_string(),
            space_ids: vec!["space-1".to_string()],
            generation_id: "gen-a".to_string(),
            snapshot_id: None,
            results: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_retrieval_recorded_inside_the_scope_reaches_the_recorder() {
        let recorder = CallRecorder::new();
        let recorded = scope(recorder.clone(), async {
            record_memory_retrieval(retrieval("a"))
        })
        .await;
        assert!(recorded);
        assert_eq!(recorder.memory_retrievals()[0].query, "a");
    }

    #[tokio::test]
    async fn recording_outside_a_scope_is_refused_and_keeps_nothing() {
        assert!(!record_memory_retrieval(retrieval("a")));
    }

    #[tokio::test]
    async fn two_scopes_do_not_see_each_others_records() {
        let (first, second) = (CallRecorder::new(), CallRecorder::new());
        let a = scope(first.clone(), async {
            record_memory_retrieval(retrieval("a"))
        });
        let b = scope(second.clone(), async {
            record_memory_retrieval(retrieval("b"))
        });
        tokio::join!(a, b);
        assert_eq!(first.memory_retrievals().len(), 1);
        assert_eq!(second.memory_retrievals()[0].query, "b");
    }
}
