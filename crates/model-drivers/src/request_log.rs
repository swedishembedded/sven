// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes whose faults can be replayed
// and reproduced on demand for its clients. If your team needs expertise in
// debugging LLM agents across the model stack then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Recording every model request exactly as sent, and what came back, so a
//! fault can be replayed from the request before it.
//!
//! [`RecordingProvider`] wraps the session's provider and appends to one
//! JSONL file: a [`Record::Request`] line when a request is sent (so a run
//! that dies mid-stream still left it), then a [`Record::Response`] line with
//! what the stream carried when it ends. `sven replay` reads the file and
//! sends a chosen request again, as often as asked, to whatever model and
//! build is current.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent};

/// One line of a request log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    /// A request as sent; `seq` counts from 1 within the file's run.
    Request {
        seq: u64,
        provider: String,
        model: String,
        request: Box<CompletionRequest>,
    },
    /// What came back for request `seq`.
    Response { seq: u64, response: Response },
}

/// A response as the stream carried it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Response {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<RecordedCall>,
    /// The seed the server reports it sampled with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    /// The output-token limit cut the response.
    pub max_tokens: bool,
    /// The stream failed with this error instead of finishing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One tool call, its streamed pieces joined.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordedCall {
    pub index: u32,
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl Response {
    /// Folds one stream event in.
    pub fn observe(&mut self, event: &ResponseEvent) {
        match event {
            ResponseEvent::TextDelta(t) => self.text.push_str(t),
            ResponseEvent::ThinkingDelta(t) => self.reasoning.push_str(t),
            ResponseEvent::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                let call = match self.tool_calls.iter_mut().find(|c| c.index == *index) {
                    Some(call) => call,
                    None => {
                        self.tool_calls.push(RecordedCall {
                            index: *index,
                            ..RecordedCall::default()
                        });
                        self.tool_calls.last_mut().expect("just pushed")
                    }
                };
                if !id.is_empty() {
                    call.id.clone_from(id);
                }
                if !name.is_empty() {
                    call.name.clone_from(name);
                }
                call.arguments.push_str(arguments);
            }
            ResponseEvent::Served { seed: Some(s), .. } => self.seed = Some(*s),
            ResponseEvent::Identity(identity) if identity.seed.is_some() => {
                self.seed = identity.seed;
            }
            ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                ..
            } => {
                self.input_tokens += input_tokens;
                self.output_tokens += output_tokens;
                self.cache_read_tokens += cache_read_tokens;
            }
            ResponseEvent::MaxTokens => self.max_tokens = true,
            ResponseEvent::Error(e) => self.error = Some(e.clone()),
            _ => {}
        }
    }
}

/// An append-only request log, shared by every request of a process.
pub struct RequestLog {
    path: PathBuf,
    file: Mutex<File>,
    next_seq: AtomicU64,
}

impl RequestLog {
    /// Opens `path` for appending, creating it.
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening the request log {}", path.display()))?;
        Ok(Arc::new(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
            next_seq: AtomicU64::new(1),
        }))
    }

    /// Appends `record` as one line. A failed write is logged, never fatal:
    /// the run matters more than its recording.
    fn append(&self, record: &Record) {
        let mut line = match serde_json::to_string(record) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "request log: cannot serialize a record");
                return;
            }
        };
        line.push('\n');
        let Ok(mut file) = self.file.lock() else {
            return;
        };
        if let Err(e) = file.write_all(line.as_bytes()) {
            tracing::warn!(path = %self.path.display(), error = %e, "request log: write failed");
        }
    }
}

/// The log every provider this process builds records to, once
/// [`install`] has opened one.
static INSTALLED: std::sync::OnceLock<Arc<RequestLog>> = std::sync::OnceLock::new();

/// Records every model request of this process to `path` from now on:
/// [`crate::from_config`] and [`crate::from_config_probed`] wrap each
/// provider they build. Installing twice is refused.
pub fn install(path: &Path) -> Result<()> {
    let log = RequestLog::open(path)?;
    INSTALLED
        .set(log)
        .map_err(|_| anyhow::anyhow!("a request log is already installed"))
}

/// `provider`, recording to the installed log if there is one.
pub(crate) fn wrap(provider: Box<dyn ModelProvider>) -> Box<dyn ModelProvider> {
    match INSTALLED.get() {
        Some(log) => Box::new(RecordingProvider::new(Arc::from(provider), Arc::clone(log))),
        None => provider,
    }
}

/// The records of a request log, in file order.
pub fn read_log(path: &Path) -> Result<Vec<Record>> {
    let file =
        File::open(path).with_context(|| format!("opening the request log {}", path.display()))?;
    BufReader::new(file)
        .lines()
        .enumerate()
        .filter(|(_, l)| l.as_ref().map_or(true, |l| !l.trim().is_empty()))
        .map(|(n, line)| {
            let line = line.with_context(|| format!("reading line {}", n + 1))?;
            serde_json::from_str(&line)
                .with_context(|| format!("{}:{}: not a request-log record", path.display(), n + 1))
        })
        .collect()
}

/// A provider that records every request and response to a [`RequestLog`].
pub struct RecordingProvider {
    inner: Arc<dyn ModelProvider>,
    log: Arc<RequestLog>,
}

impl RecordingProvider {
    #[must_use]
    pub fn new(inner: Arc<dyn ModelProvider>, log: Arc<RequestLog>) -> Self {
        Self { inner, log }
    }
}

type EventStream = sven_model::ResponseStream;

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ModelProvider for RecordingProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn complete(&self, req: CompletionRequest) -> Result<EventStream> {
        let seq = self.log.next_seq.fetch_add(1, Ordering::Relaxed);
        self.log.append(&Record::Request {
            seq,
            provider: self.inner.name().to_string(),
            model: self.inner.model_name().to_string(),
            request: Box::new(req.clone()),
        });
        let log = Arc::clone(&self.log);
        let stream = match self.inner.complete(req).await {
            Ok(s) => s,
            Err(e) => {
                let response = Response {
                    error: Some(format!("{e:#}")),
                    ..Response::default()
                };
                log.append(&Record::Response { seq, response });
                return Err(e);
            }
        };
        // The response line is written when the stream ends, however it
        // ends: finished, failed, or dropped by the consumer.
        let mut recorder = Recorder {
            log,
            seq,
            response: Response::default(),
        };
        Ok(Box::pin(stream.map(move |item| {
            match &item {
                Ok(event) => recorder.response.observe(event),
                Err(e) => recorder.response.error = Some(format!("{e:#}")),
            }
            item
        })))
    }

    async fn list_models(&self) -> Result<Vec<sven_model::ModelCatalogEntry>> {
        self.inner.list_models().await
    }

    async fn discover(&self) -> Option<sven_model::ServedModels> {
        self.inner.discover().await
    }

    fn take_model_change(&self) -> Option<sven_model::ModelChange> {
        self.inner.take_model_change()
    }

    fn tool_support(&self) -> Option<bool> {
        self.inner.tool_support()
    }

    fn supports_tool_choice_none(&self) -> bool {
        self.inner.supports_tool_choice_none()
    }

    fn endpoint_host(&self) -> Option<String> {
        self.inner.endpoint_host()
    }

    fn is_local(&self) -> bool {
        self.inner.is_local()
    }

    fn catalog_max_output_tokens(&self) -> Option<u32> {
        self.inner.catalog_max_output_tokens()
    }

    fn catalog_context_window(&self) -> Option<u32> {
        self.inner.catalog_context_window()
    }

    fn config_context_window(&self) -> Option<u32> {
        self.inner.config_context_window()
    }

    fn config_max_output_tokens(&self) -> Option<u32> {
        self.inner.config_max_output_tokens()
    }

    async fn probe_context_window(&self) -> Option<u32> {
        self.inner.probe_context_window().await
    }

    fn input_modalities(&self) -> Vec<sven_model::catalog::InputModality> {
        self.inner.input_modalities()
    }
}

/// Writes the response line when the stream it rides on is dropped.
struct Recorder {
    log: Arc<RequestLog>,
    seq: u64,
    response: Response,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.log.append(&Record::Response {
            seq: self.seq,
            response: std::mem::take(&mut self.response),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::Message;

    struct Canned;

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl ModelProvider for Canned {
        fn name(&self) -> &str {
            "canned"
        }
        fn model_name(&self) -> &str {
            "m"
        }
        async fn complete(&self, _req: CompletionRequest) -> Result<EventStream> {
            let events = vec![
                Ok(ResponseEvent::TextDelta("Let me look:".into())),
                Ok(ResponseEvent::ToolCall {
                    index: 0,
                    id: "c1".into(),
                    name: "shell".into(),
                    arguments: "{\"command\":".into(),
                }),
                Ok(ResponseEvent::ToolCall {
                    index: 0,
                    id: String::new(),
                    name: String::new(),
                    arguments: "\"ls\"}".into(),
                }),
                Ok(ResponseEvent::Done),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn a_request_and_its_response_are_recorded_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests.jsonl");
        let provider = RecordingProvider::new(Arc::new(Canned), RequestLog::open(&path).unwrap());
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            ..CompletionRequest::default()
        };
        let stream = provider.complete(req).await.unwrap();
        let _: Vec<_> = stream.collect().await;

        let records = read_log(&path).unwrap();
        let Record::Request { seq, request, .. } = &records[0] else {
            panic!("{records:?}")
        };
        assert_eq!((*seq, request.messages[0].as_text()), (1, Some("hi")));
        let Record::Response { response, .. } = &records[1] else {
            panic!("{records:?}")
        };
        assert_eq!(response.text, "Let me look:");
        assert_eq!(response.tool_calls[0].arguments, "{\"command\":\"ls\"}");
    }
}
