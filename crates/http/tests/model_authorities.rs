// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A model server behind a certificate authority an application made for it
//! is trusted by model clients once that authority is added, and not before:
//! certificates stay verified. The authority is a throwaway made in memory,
//! the server listens on the loopback under its address, and this test has a
//! binary of its own, since the authorities are the process's.

use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::TlsAcceptor;

/// A TLS server on 127.0.0.1 and the PEM of the authority it chains to.
async fn server() -> (u16, String) {
    sven_http::ensure_crypto_provider();
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["127.0.0.1".to_string()])
        .unwrap()
        .signed_by(&leaf_key, &Issuer::new(ca_params, ca_key))
        .unwrap();
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
    (port, ca.pem())
}

#[tokio::test]
async fn a_model_client_trusts_an_added_authority_and_nothing_before_it() {
    let (port, ca) = server().await;
    let url = format!("https://127.0.0.1:{port}/v1/models");

    let before = sven_http::model_client_builder().build().unwrap();
    let refused = before.get(&url).send().await.unwrap_err();
    assert!(
        sven_http::describe(&refused)
            .to_lowercase()
            .contains("certificate"),
        "{}",
        sven_http::describe(&refused)
    );

    assert!(sven_http::trust_model_authorities(b"not a certificate").is_err());
    assert_eq!(
        sven_http::trust_model_authorities(ca.as_bytes()).unwrap(),
        1
    );

    let after = sven_http::model_client_builder().build().unwrap();
    let body = after.get(&url).send().await.unwrap().text().await.unwrap();
    assert_eq!(body, "ok");
}
