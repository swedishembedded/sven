// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that talk to network
// services safely. If your team needs expertise in giving autonomous agents
// controlled API access then you can procure our services by sending an
// email to info@swedishembedded.com.
//! `http_request`: one HTTP call with a chosen method, headers and body, for
//! talking to an API (a pub/sub topic, a paste service, a JSON-RPC peer).
//!
//! `web_fetch` is the read-only page reader; this tool is its general form.
//! The same client enforces the egress policy and the public-only rule, the
//! body of the answer is read as a stream and abandoned at the limit, and the
//! call and its result are in the trajectory like any tool call.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Method;
use serde_json::{json, Value};

use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};
use sven_vocab::AgentMode;

/// Response bytes read when the caller names no limit.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;
/// Most response bytes a call may ask for.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Largest request body.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;
const MAX_HEADERS: usize = 32;

/// Headers the transport owns: setting them would break framing or let a
/// call pose as another destination.
const TRANSPORT_HEADERS: [&str; 5] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
];

#[derive(Debug, Clone, Default)]
pub struct HttpRequestTool {
    public_only: bool,
}

impl HttpRequestTool {
    /// The same tool, reaching only the public internet.
    #[must_use]
    pub fn public_only(mut self) -> Self {
        self.public_only = true;
        self
    }
}

#[async_trait]
impl Tool for HttpRequestTool {
    fn name(&self) -> &str {
        "http_request"
    }

    fn summary(&self) -> &str {
        "Send an HTTP request (any method, headers, body) to an API and read the answer."
    }

    fn description(&self) -> &str {
        "Send one HTTP(S) request; returns status, headers and the body up to max_bytes. \
         For APIs (pub/sub topics, pastes, JSON-RPC). Any status is an answer; a refused \
         or unreachable host is an error."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string" },
                "method": {
                    "type": "string",
                    "enum": ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"]
                },
                "headers": {
                    "type": "object",
                    "additionalProperties": { "type": "string" }
                },
                "body": { "type": "string", "description": "at most 1 MiB" },
                "max_bytes": { "type": "integer", "description": "default 65536, at most 4 MiB" },
                "timeout_secs": { "type": "integer", "description": "default 30, at most 120" }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::NetworkAccess
    }

    /// A POST changes the world: never in a mode that promises not to write.
    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match send(&call.args, self.public_only).await {
            Ok(text) => ToolOutput::ok(&call.id, text),
            Err(e) => ToolOutput::err(&call.id, format!("http_request error: {e}")),
        }
    }
}

struct Request {
    method: Method,
    url: reqwest::Url,
    headers: HeaderMap,
    body: Option<String>,
    max_bytes: usize,
    timeout: Duration,
}

fn parse(args: &Value, public_only: bool) -> anyhow::Result<Request> {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing 'url'"))?;
    let url = crate::client::checked_url(url, public_only)?;
    let method = args.get("method").and_then(Value::as_str).unwrap_or("GET");
    let method = match method.to_ascii_uppercase().as_str() {
        m @ ("GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE") => {
            Method::from_bytes(m.as_bytes())?
        }
        other => anyhow::bail!("unsupported method `{other}`"),
    };
    let mut headers = HeaderMap::new();
    if let Some(given) = args.get("headers") {
        let given = given
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("'headers' must be an object of strings"))?;
        if given.len() > MAX_HEADERS {
            anyhow::bail!("at most {MAX_HEADERS} headers");
        }
        for (name, value) in given {
            let lower = name.to_ascii_lowercase();
            if TRANSPORT_HEADERS.contains(&lower.as_str()) {
                anyhow::bail!("header `{name}` is set by the transport and cannot be given");
            }
            let value = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("header `{name}` must be a string"))?;
            headers.insert(
                HeaderName::try_from(name.as_str())?,
                HeaderValue::try_from(value)?,
            );
        }
    }
    let body = args.get("body").and_then(Value::as_str).map(str::to_owned);
    if body.as_ref().is_some_and(|b| b.len() > MAX_REQUEST_BYTES) {
        anyhow::bail!("body larger than {MAX_REQUEST_BYTES} bytes");
    }
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_MAX_BYTES, |n| {
            usize::try_from(n).unwrap_or(usize::MAX)
        })
        .clamp(1, MAX_RESPONSE_BYTES);
    let timeout = Duration::from_secs(
        args.get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS),
    );
    Ok(Request {
        method,
        url,
        headers,
        body,
        max_bytes,
        timeout,
    })
}

async fn send(args: &Value, public_only: bool) -> anyhow::Result<String> {
    let request = parse(args, public_only)?;
    let client = crate::client::web_client(request.timeout, public_only)?;
    let mut builder = client
        .request(request.method, request.url)
        .headers(request.headers);
    if let Some(body) = request.body {
        builder = builder.body(body);
    }
    let mut response = builder
        .send()
        .await
        .map_err(|e| crate::client::request_error(&e))?;

    let mut text = format!("{:?} {}\n", response.version(), response.status());
    for (name, value) in response.headers() {
        text.push_str(&format!(
            "{}: {}\n",
            name,
            String::from_utf8_lossy(value.as_bytes())
        ));
    }
    text.push('\n');

    // Read as a stream and stop at the limit: the rest of an endless or huge
    // answer is never downloaded.
    let mut body = Vec::new();
    let mut cut = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| crate::client::request_error(&e))?
    {
        let room = request.max_bytes - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            cut = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    match String::from_utf8(body) {
        Ok(utf8) => text.push_str(&utf8),
        Err(e) => {
            text.push_str(&String::from_utf8_lossy(e.as_bytes()));
            text.push_str("\n[body is not valid UTF-8; shown lossily]");
        }
    }
    if cut {
        text.push_str(&format!(
            "\n[truncated: the answer is larger than max_bytes {}]",
            request.max_bytes
        ));
    }
    Ok(text)
}

impl ToolDisplay for HttpRequestTool {
    fn display_name(&self) -> &str {
        "HttpRequest"
    }
    fn icon(&self) -> &str {
        "🔗"
    }
    fn category(&self) -> &str {
        "web"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        let method = args.get("method").and_then(Value::as_str).unwrap_or("GET");
        let url = args.get("url").and_then(Value::as_str).unwrap_or("");
        format!("{method} {url}")
    }
}

#[cfg(test)]
mod tests;
