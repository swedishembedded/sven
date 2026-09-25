// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! The exact input the model was given, captured where it provably exists.
//!
//! Deriving training data needs the whole thing a model was shown: the system
//! prompt, the tool schemas, and the conversation. An agent's stored history
//! is only the last of those - the system prompt and the tools are assembled
//! per turn and never kept - so a dataset built from the history alone trains
//! the model in a context it never meets at inference. The chat template
//! renders a tools preamble when tools are present and does not when they are
//! absent, so the mismatch is not subtle; it is a different prompt.
//!
//! Rather than reconstruct what was probably sent, this records what was
//! actually sent. The sample already names its endpoint, so pointing the agent
//! at a relay on the way to that endpoint costs nothing and yields ground
//! truth: the same bytes the server parsed.
//!
//! # Why a byte relay and not an HTTP client
//!
//! Completions stream. Once the request is captured there is nothing to
//! interpret in the response, and anything that parsed it would have to
//! understand server-sent events to hand them back unchanged. Copying bytes
//! cannot corrupt what it does not read.
//!
//! `Connection: close` is forced on the way out so one connection carries one
//! request. Keep-alive would put several requests on a socket with streamed
//! responses in between, and finding the boundaries would mean parsing the
//! responses - the thing this deliberately does not do.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

/// A relay that records every request body it passes on.
pub struct Recorder {
    port: u16,
    captures: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Recorder {
    /// Start relaying to `upstream` (host:port), listening on an ephemeral
    /// local port.
    pub async fn start(upstream: &str) -> std::io::Result<Recorder> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let captures: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));

        let upstream = upstream.to_string();
        let sink = Arc::clone(&captures);
        tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    break;
                };
                let upstream = upstream.clone();
                let sink = Arc::clone(&sink);
                tokio::spawn(async move {
                    let _ = relay(client, &upstream, sink).await;
                });
            }
        });

        Ok(Recorder { port, captures })
    }

    /// The base URL an agent should be pointed at.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    /// Every request body seen so far, in order.
    pub async fn captured(&self) -> Vec<serde_json::Value> {
        self.captures.lock().await.clone()
    }

    /// The tool schemas the agent sent, taken from the first request that
    /// carried any.
    ///
    /// This is the array a training record needs in order to be rendered by
    /// the same template inference uses. Empty means the agent sent no tools,
    /// which is a fact about the run and not a default to paper over.
    pub async fn tool_schemas(&self) -> Vec<serde_json::Value> {
        for request in self.captures.lock().await.iter() {
            if let Some(tools) = request.get("tools").and_then(|t| t.as_array()) {
                if !tools.is_empty() {
                    return tools.clone();
                }
            }
        }
        Vec::new()
    }

    /// Write every captured request beside an episode, for auditing what the
    /// model was actually asked.
    pub async fn dump(&self, path: &Path) -> std::io::Result<()> {
        let body = serde_json::to_string_pretty(&self.captured().await)
            .unwrap_or_else(|e| format!("[\"unserialisable: {e}\"]"));
        std::fs::write(path, body)
    }
}

async fn relay(
    mut client: TcpStream,
    upstream: &str,
    sink: Arc<Mutex<Vec<serde_json::Value>>>,
) -> std::io::Result<()> {
    // Read the head, then exactly Content-Length bytes of body. Nothing here
    // interprets the response.
    let mut buffer = Vec::new();
    let head_end = loop {
        let mut chunk = [0u8; 4096];
        let read = client.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = find(&buffer, b"\r\n\r\n") {
            break at + 4;
        }
    };

    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let length = content_length(&head).unwrap_or(0);
    while buffer.len() < head_end + length {
        let mut chunk = [0u8; 8192];
        let read = client.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    let body = &buffer[head_end..(head_end + length).min(buffer.len())];
    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) {
        sink.lock().await.push(json);
    }

    let mut server = TcpStream::connect(upstream).await?;
    server.write_all(rewrite_head(&head).as_bytes()).await?;
    server.write_all(body).await?;
    server.flush().await?;

    // From here it is bytes in both directions until either side is done.
    let (mut cr, mut cw) = client.split();
    let (mut sr, mut sw) = server.split();
    let to_server = tokio::io::copy(&mut cr, &mut sw);
    let to_client = tokio::io::copy(&mut sr, &mut cw);
    let _ = tokio::join!(to_server, to_client);
    Ok(())
}

/// Force one request per connection. See the module docs.
fn rewrite_head(head: &str) -> String {
    let mut out = String::with_capacity(head.len() + 24);
    for line in head.split_inclusive("\r\n") {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("connection:") || lower.starts_with("proxy-connection:") {
            continue;
        }
        if line == "\r\n" {
            out.push_str("Connection: close\r\n\r\n");
            return out;
        }
        out.push_str(line);
    }
    out
}

fn content_length(head: &str) -> Option<usize> {
    head.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Where a base URL's host and port are, for the relay's upstream.
pub fn upstream_of(base_url: &str) -> Option<String> {
    let rest = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))?;
    let authority = rest.split('/').next()?;
    if authority.contains(':') {
        Some(authority.to_string())
    } else {
        Some(format!("{authority}:80"))
    }
}

/// Where captures are written beside an episode.
pub fn capture_path(dir: &Path) -> PathBuf {
    dir.join("requests.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upstream_is_taken_from_the_base_url() {
        assert_eq!(
            upstream_of("http://127.0.0.1:8791/v1").as_deref(),
            Some("127.0.0.1:8791")
        );
        assert_eq!(
            upstream_of("http://localhost/v1").as_deref(),
            Some("localhost:80")
        );
        assert_eq!(upstream_of("not a url"), None);
    }

    #[test]
    fn the_forwarded_head_carries_exactly_one_connection_header() {
        let head =
            "POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nConnection: keep-alive\r\n\r\n";
        let rewritten = rewrite_head(head);
        assert_eq!(rewritten.matches("Connection:").count(), 1, "{rewritten}");
        assert!(rewritten.contains("Connection: close"), "{rewritten}");
        assert!(
            rewritten.contains("Host: x"),
            "other headers must survive: {rewritten}"
        );
    }

    #[test]
    fn a_content_length_is_read_case_insensitively() {
        assert_eq!(
            content_length("POST / HTTP/1.1\r\ncontent-length: 42\r\n\r\n"),
            Some(42)
        );
        assert_eq!(
            content_length("POST / HTTP/1.1\r\nContent-Length:  7\r\n\r\n"),
            Some(7)
        );
        assert_eq!(content_length("POST / HTTP/1.1\r\n\r\n"), None);
    }

    #[tokio::test]
    async fn the_recorder_captures_the_body_and_relays_the_answer() {
        // A stand-in upstream: read a request, reply with a fixed body.
        let upstream = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = upstream.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut server, _) = upstream.accept().await.expect("accept");
            let mut seen = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = server.read(&mut chunk).await.expect("read");
                seen.extend_from_slice(&chunk[..read]);
                if read == 0 || seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            server
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .expect("write");
        });

        let recorder = Recorder::start(&addr.to_string()).await.expect("recorder");
        let body = r#"{"model":"m","tools":[{"name":"shell"}],"messages":[]}"#;
        let mut client = TcpStream::connect(format!("127.0.0.1:{}", recorder.port))
            .await
            .expect("connect");
        client
            .write_all(
                format!(
                    "POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .expect("write");

        let mut answer = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut answer),
        )
        .await;

        let captured = recorder.captured().await;
        assert_eq!(captured.len(), 1, "the request body must be recorded");
        assert_eq!(captured[0]["model"], "m");
        assert_eq!(
            recorder.tool_schemas().await.len(),
            1,
            "the tool schemas are what a training record needs"
        );
        assert!(
            String::from_utf8_lossy(&answer).contains("ok"),
            "the upstream's answer must reach the caller unchanged"
        );
    }
}
