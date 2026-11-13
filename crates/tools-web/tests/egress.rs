// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! With an egress policy installed, every network tool reaches the listed
//! local server and nothing else. This binary installs one policy for all its
//! tests (a process has one), allowing a local server and the name
//! `service.test`, which resolves to loopback. Nothing here leaves the
//! machine: refused destinations are never connected to, which the counting
//! server proves.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use sven_http::egress::{EgressPolicy, NetworkConfig};
use sven_tool_api::tool::{Tool, ToolCall};
use sven_tools_web::{HttpRequestTool, WebFetchTool, WebSearchTool};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct World {
    allowed: SocketAddr,
    /// A second local server the policy does not list.
    forbidden: SocketAddr,
    forbidden_hits: Arc<AtomicUsize>,
}

async fn serve(hits: Arc<AtomicUsize>, body: &'static str) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            hits.fetch_add(1, Ordering::SeqCst);
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b).await;
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(reply.as_bytes()).await;
        }
    });
    addr
}

/// The servers live on a runtime of their own thread, so they outlive any
/// one test's runtime.
fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let world = rt.block_on(async {
                let forbidden_hits = Arc::new(AtomicUsize::new(0));
                let allowed = serve(Arc::new(AtomicUsize::new(0)), "allowed body").await;
                let forbidden = serve(forbidden_hits.clone(), "SECRET").await;
                World {
                    allowed,
                    forbidden,
                    forbidden_hits,
                }
            });
            let policy = EgressPolicy::from_config(&NetworkConfig {
                allow_hosts: Some(vec![
                    format!("127.0.0.1:{}", world.allowed.port()),
                    "service.test".into(),
                ]),
                resolve: [("service.test".to_string(), "127.0.0.1".parse().unwrap())].into(),
                ..NetworkConfig::default()
            })
            .unwrap();
            sven_http::egress::install(policy).unwrap();
            tx.send(world).unwrap();
            // Park: dropping the runtime would stop the servers.
            std::thread::park();
        });
        rx.recv().unwrap()
    })
}

fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: "1".into(),
        name: name.into(),
        args,
    }
}

#[tokio::test]
async fn web_fetch_reaches_the_allowed_server_and_refuses_the_other() {
    let w = world();
    let fetch = WebFetchTool::default();
    let ok = fetch
        .execute(&call(
            "web_fetch",
            json!({"url": format!("http://{}/", w.allowed)}),
        ))
        .await;
    assert!(
        !ok.is_error && ok.content.contains("allowed body"),
        "{}",
        ok.content
    );

    let before = w.forbidden_hits.load(Ordering::SeqCst);
    let no = fetch
        .execute(&call(
            "web_fetch",
            json!({"url": format!("http://{}/", w.forbidden)}),
        ))
        .await;
    assert!(
        no.is_error && no.content.contains("network.allow_hosts"),
        "{}",
        no.content
    );
    assert_eq!(
        w.forbidden_hits.load(Ordering::SeqCst),
        before,
        "a refused fetch connected"
    );
}

#[tokio::test]
async fn http_request_refuses_every_destination_outside_the_list() {
    let w = world();
    let tool = HttpRequestTool::default();
    let before = w.forbidden_hits.load(Ordering::SeqCst);
    for url in [
        format!("http://{}/", w.forbidden),
        "https://example.com/".to_string(),
        "http://169.254.169.254/latest/meta-data/".to_string(),
        "http://localhost:1/".to_string(),
    ] {
        let out = tool
            .execute(&call(
                "http_request",
                json!({"url": url, "method": "POST", "body": "x"}),
            ))
            .await;
        assert!(
            out.is_error && out.content.contains("refused"),
            "{url}: {}",
            out.content
        );
    }
    assert_eq!(w.forbidden_hits.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn a_familiar_name_resolves_to_the_local_emulator() {
    let w = world();
    let out = HttpRequestTool::default()
        .execute(&call(
            "http_request",
            json!({"url": format!("http://service.test:{}/", w.allowed.port())}),
        ))
        .await;
    // The name is allowed (no port rule) and mapped to loopback.
    assert!(
        !out.is_error && out.content.contains("allowed body"),
        "{}",
        out.content
    );
}

#[tokio::test]
async fn a_search_provider_outside_the_list_is_refused() {
    world();
    let out = WebSearchTool::brave(Some("key".into()))
        .execute(&call("web_search", json!({"query": "anything"})))
        .await;
    assert!(
        out.is_error && out.content.contains("api.search.brave.com"),
        "{}",
        out.content
    );
}
