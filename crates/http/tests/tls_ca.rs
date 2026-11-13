// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A custom CA bundle makes a TLS server with an emulated name trusted, and
//! only then: certificates stay verified. The fixtures are a throwaway CA and
//! a leaf for `ntfy.sh` (valid 100 years, CA key discarded) that protect
//! nothing; they exist so the test needs no network and no certificate tool.

use std::collections::BTreeMap;
use std::sync::Arc;

use sven_http::egress::{EgressPolicy, NetworkConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

async fn tls_server() -> u16 {
    sven_http::ensure_crypto_provider();
    let certs = CertificateDer::pem_file_iter(format!("{FIXTURES}/ntfy-leaf.pem"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_file(format!("{FIXTURES}/ntfy-leaf.key.pem")).unwrap();
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
    port
}

fn policy(ca: bool) -> EgressPolicy {
    let mut resolve = BTreeMap::new();
    resolve.insert("ntfy.sh".to_string(), "127.0.0.1".parse().unwrap());
    EgressPolicy::from_config(&NetworkConfig {
        allow_hosts: Some(vec!["ntfy.sh".into()]),
        ca_bundle: ca.then(|| format!("{FIXTURES}/test-ca.pem").into()),
        resolve,
        ..NetworkConfig::default()
    })
    .unwrap()
}

#[tokio::test]
async fn the_bundle_authority_is_trusted_and_without_it_the_certificate_is_not() {
    let port = tls_server().await;
    let url = format!("https://ntfy.sh:{port}/");
    sven_http::ensure_crypto_provider();

    let trusting = policy(true)
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

    let strict = policy(false)
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
