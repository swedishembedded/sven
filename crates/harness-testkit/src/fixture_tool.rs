// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The fixture tool: a [`Tool`] whose answers come from a case manifest.
//!
//! It records every invocation (with the capability it exercised), tracks the
//! calls still in flight with a drop guard - so a call abandoned by a
//! cancellation is seen to end - and reports a response queue that ran dry as
//! a fixture error rather than inventing an answer.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sven_sdk::tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput};

use crate::case::{ToolFixture, ToolResponse};
use crate::dir::jail_join;

/// What the fixture tools of one run saw, shared across all of them.
#[derive(Default)]
pub(crate) struct ToolBooks {
    invoked: Mutex<Vec<(String, ToolCapability)>>,
    problems: Mutex<Vec<String>>,
    in_flight: AtomicUsize,
}

impl ToolBooks {
    /// Names of executed fixture calls, sorted.
    pub(crate) fn invoked_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.lock_invoked().iter().map(|(n, _)| n.clone()).collect();
        names.sort();
        names
    }

    /// Capabilities of executed fixture calls.
    pub(crate) fn invoked_capabilities(&self) -> Vec<ToolCapability> {
        self.lock_invoked().iter().map(|(_, c)| *c).collect()
    }

    /// Fixture problems: exhausted response queues, unusable writes.
    pub(crate) fn problems(&self) -> Vec<String> {
        self.problems.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// Calls begun and not yet ended or dropped.
    pub(crate) fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    fn lock_invoked(&self) -> Vec<(String, ToolCapability)> {
        self.invoked.lock().map(|i| i.clone()).unwrap_or_default()
    }

    fn problem(&self, message: String) {
        if let Ok(mut problems) = self.problems.lock() {
            problems.push(message);
        }
    }
}

/// Decrements the in-flight count when the call ends or is dropped.
struct InFlight(Arc<ToolBooks>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A tool answering from its manifest.
pub(crate) struct FixtureTool {
    name: String,
    capability: ToolCapability,
    responses: Mutex<VecDeque<ToolResponse>>,
    workspace: PathBuf,
    books: Arc<ToolBooks>,
}

impl FixtureTool {
    pub(crate) fn new(
        name: &str,
        fixture: &ToolFixture,
        workspace: PathBuf,
        books: Arc<ToolBooks>,
    ) -> Self {
        Self {
            name: name.to_string(),
            capability: fixture.capability,
            responses: Mutex::new(fixture.responses.iter().cloned().collect()),
            workspace,
            books,
        }
    }

    fn next_response(&self) -> Option<ToolResponse> {
        self.responses.lock().ok()?.pop_front()
    }
}

#[async_trait::async_trait]
impl Tool for FixtureTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "a fixture tool answering from a case manifest"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "additionalProperties": true})
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn kernel_capability(&self) -> ToolCapability {
        self.capability
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        self.books.in_flight.fetch_add(1, Ordering::SeqCst);
        let _in_flight = InFlight(Arc::clone(&self.books));
        if let Ok(mut invoked) = self.books.invoked.lock() {
            invoked.push((self.name.clone(), self.capability));
        }
        match self.next_response() {
            None => {
                let message = format!(
                    "fixture exhausted: tool `{}` was called more often than its responses",
                    self.name
                );
                self.books.problem(message.clone());
                ToolOutput::err(&call.id, message)
            }
            Some(ToolResponse::Error(message)) => ToolOutput::err(&call.id, message),
            Some(ToolResponse::Block) => {
                std::future::pending::<()>().await;
                unreachable!("a blocked fixture call never resumes")
            }
            Some(ToolResponse::Ok { value, write }) => {
                if let Some(write) = write {
                    if let Err(message) = self.write_into_workspace(&write.path, &write.content) {
                        self.books.problem(message.clone());
                        return ToolOutput::err(&call.id, message);
                    }
                }
                let text = match value {
                    Value::String(text) => text,
                    other => other.to_string(),
                };
                ToolOutput::ok(&call.id, text)
            }
        }
    }
}

impl FixtureTool {
    fn write_into_workspace(&self, rel: &str, content: &str) -> Result<(), String> {
        let path =
            jail_join(&self.workspace, rel).map_err(|e| format!("fixture write refused: {e}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("fixture write failed: {e}"))?;
        }
        std::fs::write(&path, content).map_err(|e| format!("fixture write failed: {e}"))
    }
}
