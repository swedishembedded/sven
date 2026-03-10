// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared WebSocket client for connecting to a running `sven node`.
//!
//! Both `sven-acp` and `sven-mcp` proxy their operations to a node over an
//! authenticated WebSocket connection. This crate provides the common TLS
//! setup and connection helper so neither proxy has to duplicate it.

use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::SinkExt;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, protocol::Message as WsMessage},
    Connector, MaybeTlsStream, WebSocketStream,
};
use tracing::debug;

pub type NodeWsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Open an authenticated WebSocket connection to a `sven node`.
///
/// `ws_url` is the full URL including scheme, e.g. `wss://127.0.0.1:18790/ws`.
/// `token` is the raw bearer token sent in the `Authorization` header.
///
/// Self-signed certificates are accepted because the node uses a locally
/// issued CA and the bearer token provides the actual authentication guarantee.
pub async fn connect(ws_url: &str, token: &str) -> Result<NodeWsStream> {
    let mut request = ws_url
        .into_client_request()
        .context("invalid WebSocket URL")?;

    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {token}")
            .parse()
            .context("invalid token header value")?,
    );

    let connector = Connector::Rustls(Arc::new(
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
            .with_no_client_auth(),
    ));

    let (stream, response) = connect_async_tls_with_config(request, None, false, Some(connector))
        .await
        .context("WebSocket connect failed")?;

    debug!(status = %response.status(), "connected to sven node");
    Ok(stream)
}

/// Send a JSON-serializable command over an open WebSocket stream.
pub async fn send_json<C: serde::Serialize>(ws: &mut NodeWsStream, cmd: &C) -> Result<()> {
    let json = serde_json::to_string(cmd).context("failed to serialise WS command")?;
    ws.send(WsMessage::Text(json))
        .await
        .context("WebSocket send failed")
}

// ── TLS: accept any certificate (bearer token is the auth mechanism) ──────────

#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
