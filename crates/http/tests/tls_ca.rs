// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A custom CA bundle makes a TLS server with an emulated name trusted, and
//! only then: certificates stay verified. Each test makes its own throwaway CA
//! and a leaf for `ntfy.sh` in memory, so no key is kept in the repository and
//! the test needs no network and no certificate tool.

use std::collections::BTreeMap;
use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use sven_http::egress::{EgressPolicy, NetworkConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::TlsAcceptor;

/// A TLS server for `ntfy.sh` signed by a CA that exists only for this test.
struct TlsServer {
    port: u16,
    /// The CA certificate (PEM) a client names as its `ca_bundle`; lives as
    /// long as the server's handle.
    ca_bundle: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn tls_server() -> TlsServer {
    sven_http::ensure_crypto_provider();
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["ntfy.sh".to_string()])
        .unwrap()
        .signed_by(&leaf_key, &Issuer::new(ca_params, ca_key))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ca_bundle = dir.path().join("test-ca.pem");
    std::fs::write(&ca_bundle, ca.pem()).unwrap();
    let certs = vec![CertificateDer::from(leaf.der().to_vec())];
    let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    let config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut buf = [0u8; 2048];
                let _ = tls.read(&mut buf).await;
                let _ = tls
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await;
                let _ = tls.shutdown().await;
            });
        }
    });
    TlsServer {
        port,
        ca_bundle,
        _dir: dir,
    }
}

fn policy(ca: Option<&std::path::Path>) -> EgressPolicy {
    let mut resolve = BTreeMap::new();
    resolve.insert("ntfy.sh".to_string(), "127.0.0.1".parse().unwrap());
    EgressPolicy::from_config(&NetworkConfig {
        allow_hosts: Some(vec!["ntfy.sh".into()]),
        ca_bundle: ca.map(Into::into),
        resolve,
        ..NetworkConfig::default()
    })
    .unwrap()
}

#[tokio::test]
async fn the_bundle_authority_is_trusted_and_without_it_the_certificate_is_not() {
    let server = tls_server().await;
    let url = format!("https://ntfy.sh:{}/", server.port);
    sven_http::ensure_crypto_provider();

    let trusting = policy(Some(&server.ca_bundle))
        .apply(reqwest::Client::builder(), None)
        .build()
        .unwrap();
    assert_eq!(
        trusting
            .get(&url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "ok"
    );

    let strict = policy(None)
        .apply(reqwest::Client::builder(), None)
        .build()
        .unwrap();
    let err = strict.get(&url).send().await.unwrap_err();
    assert!(
        sven_http::describe(&err)
            .to_lowercase()
            .contains("certificate"),
        "{}",
        sven_http::describe(&err)
    );
}

/// A local CONNECT proxy that demands `Proxy-Authorization: Basic dTpw`
/// (`u:p`): answers 407 without it, otherwise tunnels to `upstream` and
/// reports the CONNECT target it was asked for.
async fn connect_proxy(upstream: u16) -> (u16, tokio::sync::mpsc::Receiver<(String, bool)>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else {
                return;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if client.read(&mut byte).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let target = head.lines().next().unwrap_or("").to_string();
                let authorised = head
                    .to_lowercase()
                    .contains("proxy-authorization: basic dtpw");
                let _ = tx.send((target, authorised)).await;
                if !authorised {
                    let _ = client
                        .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\ncontent-length: 0\r\n\r\n")
                        .await;
                    return;
                }
                let Ok(mut server) = tokio::net::TcpStream::connect(("127.0.0.1", upstream)).await
                else {
                    return;
                };
                let _ = client
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await;
                let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
            });
        }
    });
    (port, rx)
}

#[tokio::test]
async fn https_goes_through_an_authenticating_connect_proxy_with_the_bundle_trusted() {
    let tls = tls_server().await;
    let (proxy, mut seen) = connect_proxy(tls.port).await;
    sven_http::ensure_crypto_provider();
    let config = |login: &str| NetworkConfig {
        allow_hosts: Some(vec!["ntfy.sh".into()]),
        proxy: Some(format!("http://{login}127.0.0.1:{proxy}")),
        ca_bundle: Some(tls.ca_bundle.clone()),
        ..NetworkConfig::default()
    };
    let client = |c: &NetworkConfig| {
        EgressPolicy::from_config(c)
            .unwrap()
            .apply(reqwest::Client::builder(), None)
            .build()
            .unwrap()
    };

    // Credentials in the proxy URL are sent as Basic; the name is the
    // destination's own, resolved by the proxy (not by this process).
    let body = client(&config("u:p@"))
        .get("https://ntfy.sh/topic")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "ok");
    assert_eq!(
        seen.recv().await.unwrap(),
        ("CONNECT ntfy.sh:443 HTTP/1.1".to_string(), true)
    );

    // Without them the proxy refuses and the request fails.
    assert!(client(&config(""))
        .get("https://ntfy.sh/topic")
        .send()
        .await
        .is_err());
    assert!(!seen.recv().await.unwrap().1);
}
