// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for hung and silently stalled model
// connections for its clients. If your team needs expertise in resilient
// agent runtimes then you can procure our services by sending an email to
// info@swedishembedded.com.
//! The real OpenAI-compatible driver against a server that accepts the
//! connection and never answers: the turn must fail at the first-byte limit,
//! naming the driver and the server, instead of hanging until someone presses
//! Esc.

use std::time::Duration;

use sven_model_drivers::{from_config, ModelConfig};
use sven_turn::{stream_turn, StallLimits, TurnLimits};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

#[tokio::test]
async fn a_server_that_accepts_and_never_answers_fails_the_turn() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Hold every accepted connection open and silent for the whole test.
    let accepted = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });

    let provider = from_config(&ModelConfig {
        provider: "sven".into(),
        name: "stalled".into(),
        base_url: Some(format!("http://{addr}/v1")),
        ..ModelConfig::default()
    })
    .unwrap();
    let limits = TurnLimits::default().with_stall_limits(StallLimits {
        first_byte: Some(Duration::from_millis(400)),
        ..StallLimits::default()
    });
    let (tx, _rx) = mpsc::channel(16);

    let err = stream_turn(
        provider.as_ref(),
        vec![],
        vec![],
        None,
        None,
        None,
        None,
        limits,
        &tx,
    )
    .await
    .expect_err("an unanswered request must fail the turn");

    assert!(
        err.to_string()
            .contains(&format!("no response headers from sven {addr} after 0.4 s")),
        "{err}"
    );
    accepted.abort();
}
