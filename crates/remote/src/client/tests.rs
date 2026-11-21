// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The HTTP client's tests: discovery, the command stream and event uploads.

use super::*;

use futures::StreamExt;
use sven_control::remote::{EventBatch, RemoteDiscovery};

use super::discovery::same_origin;
use super::environment::decode_command;

/// A host's uploads go compressed once the server says it takes gzip, and
/// plainly again, for good, once one is refused (415): an older server
/// is never sent a body it cannot read.
#[tokio::test]
async fn event_uploads_go_compressed_once_the_server_takes_them() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (seen, mut encodings) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let (mut line, mut length, mut encoding) = (String::new(), 0usize, None);
            reader.read_line(&mut line).await.unwrap();
            loop {
                line.clear();
                reader.read_line(&mut line).await.unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = line.trim().split_once(": ") {
                    if k.eq_ignore_ascii_case("content-length") {
                        length = v.parse().unwrap();
                    }
                    if k.eq_ignore_ascii_case("content-encoding") {
                        encoding = Some(v.to_string());
                    }
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).await.unwrap();
            let status = if encoding.is_some() {
                "415 Unsupported Media Type"
            } else {
                "200 OK"
            };
            let _ = seen.send(encoding);
            let reply = format!(
                "HTTP/1.1 {status}\r\nAccept-Encoding: zstd, gzip\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = write.write_all(reply.as_bytes()).await;
        }
    });
    let client = RemoteClient::new(&format!("{base}/api"), "token").unwrap();
    let batch: EventBatch =
        serde_json::from_value(serde_json::json!({ "run": Uuid::nil() })).unwrap();
    client.post_events(&batch).await.unwrap();
    client.post_events(&batch).await.unwrap();
    client.post_events(&batch).await.unwrap();
    let mut got = Vec::new();
    for _ in 0..4 {
        got.push(encodings.recv().await.unwrap());
    }
    assert_eq!(
        got,
        [None, Some("gzip".to_string()), None, None],
        "plain, then compressed (refused) and sent again plainly, then plainly for good"
    );
}

/// Nothing a sign-in sends goes off the server's origin: the control and
/// model APIs, the MCP endpoint and where its tokens are traded.
#[test]
fn every_url_a_sign_in_sends_to_is_on_the_servers_origin() {
    let found = |mcp_token_url: &str| -> RemoteDiscovery {
        serde_json::from_value(serde_json::json!({
            "protocol_version": 1,
            "authorize_url": "https://s/connect",
            "control_url": "https://s/api/workspace",
            "openai_base_url": "https://s/v1",
            "mcp_url": "https://s/api/mcp",
            "mcp_token_url": mcp_token_url,
        }))
        .unwrap()
    };
    assert!(ensure_on_server("https://s", &found("https://s/api/mcp/token"), &[], "it").is_ok());
    let refused =
        ensure_on_server("https://s", &found("https://evil.example/token"), &[], "it").unwrap_err();
    assert!(refused.to_string().contains("mcp_token_url"), "{refused}");
    assert!(ensure_on_server(
        "https://s",
        &found("https://s/t"),
        &[("device_token_url", "https://x/t")],
        "it"
    )
    .is_err());
}

#[test]
fn a_bare_address_gets_the_scheme_its_host_needs() {
    assert_eq!(
        normalize_server("localhost:3000").unwrap(),
        "http://localhost:3000"
    );
    assert_eq!(
        normalize_server("127.0.0.1:3000/").unwrap(),
        "http://127.0.0.1:3000"
    );
    assert_eq!(
        normalize_server("example.com").unwrap(),
        "https://example.com"
    );
    assert_eq!(
        normalize_server("https://example.com/").unwrap(),
        "https://example.com"
    );
    assert!(normalize_server("ftp://example.com").is_err());
}

#[test]
fn an_unreadable_command_still_says_where_it_was() {
    let poison = decode_command(r#"{"seq":7,"command":{"type":"from_the_future"}}"#).unwrap_err();
    assert_eq!(
        poison.downcast_ref::<UnreadableCommand>().unwrap().seq,
        Some(7)
    );
    let garbage = decode_command("{").unwrap_err();
    assert_eq!(
        garbage.downcast_ref::<UnreadableCommand>().unwrap().seq,
        None
    );
    assert!(decode_command(r#"{"seq":1,"command":{"type":"list_slash_commands"}}"#).is_ok());
}

#[test]
fn plain_http_is_for_this_machine_only() {
    for local in [
        "http://localhost:3000",
        "http://127.0.0.1:3000",
        "http://127.1.2.3",
        "http://[::1]:3000",
        "[::1]:3000",
    ] {
        assert!(normalize_server(local).is_ok(), "{local}");
    }
    for remote in [
        "http://example.com",
        "http://192.168.1.5:3000",
        "http://localhost.example.com",
        "http://127.0.0.1.example.com",
        "http://localhost@evil.example",
    ] {
        assert!(normalize_server(remote).is_err(), "{remote}");
    }
}

#[test]
fn only_the_servers_own_origin_is_the_servers() {
    assert!(same_origin("https://s.example", "https://s.example/api"));
    assert!(same_origin("https://s.example", "https://s.example:443/v1"));
    for other in [
        "http://s.example/api",
        "https://evil.example/api",
        "https://s.example:8443/api",
        "https://s.example.evil.example/api",
        "not a url",
    ] {
        assert!(!same_origin("https://s.example", other), "{other}");
    }
    assert!(normalize_server("  ").is_err());
}

/// A server on a loopback port that answers one request by writing
/// `parts`, each after its delay, then holds the connection open without
/// a word: what a dead peer behind a NAT or a stopped proxy looks like.
async fn server_writing(parts: Vec<(Duration, &'static str)>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        for (delay, text) in parts {
            tokio::time::sleep(delay).await;
            if socket.write_all(text.as_bytes()).await.is_err() {
                return;
            }
        }
        std::future::pending::<()>().await;
    });
    format!("http://{addr}/api/workspace")
}

const SSE_HEADERS: &str =
    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
const KEEPALIVE: &str = ": keep-alive\n\n";
const NOW: Duration = Duration::ZERO;

fn stream_limits() -> StreamLimits {
    StreamLimits {
        open: Duration::from_millis(300),
        idle: Duration::from_millis(300),
    }
}

/// A server that takes the request and never answers is given up on, so
/// the host reconnects instead of waiting on it for good.
#[tokio::test]
async fn a_command_stream_that_never_opens_is_given_up_on() {
    let client = RemoteClient::new(&server_writing(vec![]).await, "t").unwrap();
    let opened = tokio::time::timeout(
        Duration::from_secs(5),
        client.commands_within(0, stream_limits()),
    )
    .await
    .expect("the wait for the stream to open is bounded");
    assert!(opened.is_err());
}

/// A stream that goes silent, keepalives included, ends with an error, so
/// the host's loop reconnects instead of waiting on a dead connection.
#[tokio::test]
async fn a_command_stream_that_goes_silent_ends() {
    let url = server_writing(vec![(NOW, SSE_HEADERS), (NOW, KEEPALIVE)]).await;
    let client = RemoteClient::new(&url, "t").unwrap();
    let stream = client.commands_within(0, stream_limits()).await.unwrap();
    futures::pin_mut!(stream);
    let next = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("a silent stream ends");
    assert!(matches!(next, Some(Err(_))), "{next:?}");
}

/// Keepalives are not commands, but they prove the server is there: a
/// stream that carries only keepalives for longer than the idle limit
/// stays open, and delivers the command that follows.
#[tokio::test]
async fn keepalives_hold_a_quiet_command_stream_open() {
    let tick = Duration::from_millis(100);
    let url = server_writing(vec![
        (NOW, SSE_HEADERS),
        (tick, KEEPALIVE),
        (tick, KEEPALIVE),
        (tick, KEEPALIVE),
        (tick, KEEPALIVE),
        (tick, KEEPALIVE),
        (
            tick,
            "data: {\"seq\":3,\"command\":{\"type\":\"list_slash_commands\"}}\n\n",
        ),
    ])
    .await;
    let client = RemoteClient::new(&url, "t").unwrap();
    let stream = client.commands_within(0, stream_limits()).await.unwrap();
    futures::pin_mut!(stream);
    let next = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("the command arrives");
    assert_eq!(next.unwrap().unwrap().seq, 3);
}
