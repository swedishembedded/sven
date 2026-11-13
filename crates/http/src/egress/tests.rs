// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::model_client_builder;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn policy(yaml_hosts: &[&str]) -> EgressPolicy {
    EgressPolicy::from_config(&NetworkConfig {
        allow_hosts: Some(yaml_hosts.iter().map(|s| (*s).to_string()).collect()),
        ..NetworkConfig::default()
    })
    .unwrap()
}

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

#[test]
fn without_a_list_nothing_is_restricted() {
    let open = EgressPolicy::from_config(&NetworkConfig::default()).unwrap();
    assert!(open.check(&url("https://anywhere.test/")).is_ok());
}

#[test]
fn a_present_list_denies_everything_it_does_not_name() {
    let p = policy(&[
        "ntfy.sh",
        "*.paste.test",
        "api.test:8443",
        "[::1]:9000",
        "10.0.0.5",
    ]);
    for ok in [
        "https://ntfy.sh/topic",
        "http://NTFY.sh/x",
        "https://a.paste.test/",
        "https://x.y.paste.test/",
        "https://api.test:8443/rpc",
        "http://[::1]:9000/",
        "http://10.0.0.5:81/",
    ] {
        assert!(p.check(&url(ok)).is_ok(), "{ok} refused");
    }
    for denied in [
        "https://example.com/",
        "https://paste.test/",
        "https://evilntfy.sh/",
        "https://ntfy.sh.evil.test/",
        "https://api.test/",
        "http://127.0.0.1/",
        "http://2130706433/",
        "http://[::1]:9001/",
    ] {
        let why = p.check(&url(denied)).unwrap_err();
        assert!(
            why.contains("refused") && why.contains("network.allow_hosts"),
            "{why}"
        );
    }
    assert!(policy(&[]).check(&url("https://ntfy.sh/")).is_err());
}

#[test]
fn a_malformed_configuration_fails_at_startup() {
    let bad = |c: NetworkConfig| EgressPolicy::from_config(&c).unwrap_err();
    assert!(bad(NetworkConfig {
        allow_hosts: Some(vec!["https://x.test".into()]),
        ..Default::default()
    })
    .contains("allow_hosts"));
    assert!(bad(NetworkConfig {
        proxy: Some("socks5://h:1".into()),
        ..Default::default()
    })
    .contains("proxy"));
    assert!(bad(NetworkConfig {
        ca_bundle: Some("/nonexistent/ca.pem".into()),
        ..Default::default()
    })
    .contains("ca_bundle"));
    let empty = tempfile::NamedTempFile::new().unwrap();
    assert!(bad(NetworkConfig {
        ca_bundle: Some(empty.path().into()),
        ..Default::default()
    })
    .contains("no certificate"));
}

/// Answers one request per connection with `reply`; returns the address and
/// the first request line it saw.
async fn server(reply: &'static str) -> (SocketAddr, tokio::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = tx.send(head.lines().next().unwrap_or("").to_string()).await;
                let body = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = stream.write_all(body.as_bytes()).await;
            });
        }
    });
    (addr, rx)
}

fn client(p: &EgressPolicy) -> reqwest::Client {
    p.apply(model_client_builder(), None).build().unwrap()
}

#[tokio::test]
async fn an_allowed_local_server_is_reached_and_everything_else_refused() {
    let (addr, mut seen) = server("hello").await;
    let p = policy(&[&format!("127.0.0.1:{}", addr.port())]);
    let c = client(&p);
    let ok = c.get(format!("http://{addr}/a")).send().await.unwrap();
    assert_eq!(ok.text().await.unwrap(), "hello");
    assert_eq!(seen.recv().await.unwrap(), "GET /a HTTP/1.1");

    // Another port on the same address, an IP literal and a name: all refused,
    // and the server never sees a connection.
    for target in [
        format!("http://127.0.0.1:{}/", addr.port().wrapping_add(1)),
        "http://localhost:1/".to_string(),
        "https://example.com/".to_string(),
    ] {
        let err = c.get(&target).send().await.unwrap_err();
        let text = crate::describe(&err);
        assert!(
            text.contains("refused") && text.contains("allow_hosts"),
            "{target}: {text}"
        );
    }
    assert!(seen.try_recv().is_err());
}

#[tokio::test]
async fn a_redirect_out_of_the_list_is_refused() {
    let (target, mut seen) = server("secret").await;
    let location: &'static str = Box::leak(
        format!("HTTP/1.1 302 Found\r\nlocation: http://{target}/\r\ncontent-length: 0\r\n\r\n")
            .into_boxed_str(),
    );
    let front = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let front_addr = front.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = front.accept().await.unwrap();
        let mut b = [0u8; 1024];
        let _ = s.read(&mut b).await;
        let _ = s.write_all(location.as_bytes()).await;
    });
    let p = policy(&[&format!("127.0.0.1:{}", front_addr.port())]);
    let err = client(&p)
        .get(format!("http://{front_addr}/"))
        .send()
        .await
        .unwrap_err();
    assert!(crate::describe(&err).contains("refused"));
    assert!(seen.try_recv().is_err());
}

#[tokio::test]
async fn a_mapped_name_reaches_the_local_emulator() {
    let (addr, _seen) = server("emulated").await;
    let mut resolve = BTreeMap::new();
    resolve.insert("ntfy.sh".to_string(), "127.0.0.1".parse().unwrap());
    let p = EgressPolicy::from_config(&NetworkConfig {
        allow_hosts: Some(vec!["ntfy.sh".into()]),
        resolve,
        ..NetworkConfig::default()
    })
    .unwrap();
    let body = client(&p)
        .get(format!("http://ntfy.sh:{}/t", addr.port()))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "emulated");
}

#[tokio::test]
async fn allowed_requests_go_through_the_proxy_and_refused_ones_never_do() {
    let (proxy, mut seen) = server("via proxy").await;
    let p = EgressPolicy::from_config(&NetworkConfig {
        allow_hosts: Some(vec!["emulated.test".into()]),
        proxy: Some(format!("http://{proxy}")),
        ..NetworkConfig::default()
    })
    .unwrap();
    let c = client(&p);
    let body = c
        .get("http://emulated.test/x")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "via proxy");
    assert_eq!(
        seen.recv().await.unwrap(),
        "GET http://emulated.test/x HTTP/1.1"
    );
    assert!(c.get("http://other.test/").send().await.is_err());
    assert!(
        seen.try_recv().is_err(),
        "a refused destination reached the proxy"
    );
}

#[tokio::test]
async fn a_second_different_policy_cannot_be_installed() {
    // The only test in this binary that installs.
    install(policy(&["a.test"])).unwrap();
    install(policy(&["a.test"])).unwrap();
    assert!(install(policy(&["b.test"])).is_err());
    assert!(check(&url("https://a.test/")).is_ok());
    assert!(check(&url("https://b.test/")).is_err());
}
