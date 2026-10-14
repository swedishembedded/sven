// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for local model serving for its
// clients. If your team needs expertise in self-hosted LLM infrastructure then
// you can procure our services by sending an email to info@swedishembedded.com.
//! A loopback model server the integration tests talk to.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use sven_model_drivers::ModelConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A loopback model server whose `/models` answer can be changed while it
/// runs. `/props` is absent (404) and chat completions are rejected with a
/// context-overflow error, like a server whose window is smaller than the
/// prompt.
pub struct Server {
    pub base_url: String,
    models: Arc<Mutex<String>>,
}

impl Server {
    pub async fn start(models: serde_json::Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let models = Arc::new(Mutex::new(models.to_string()));
        let served = Arc::clone(&models);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let served = Arc::clone(&served);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 16384];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let line = head.lines().next().unwrap_or_default().to_string();
                    let (status, body) = if line.starts_with("GET /v1/models") {
                        (200, served.lock().unwrap().clone())
                    } else if line.starts_with("POST /v1/chat/completions") {
                        (
                            400,
                            r#"{"error":{"message":"request exceeds the available context size (n_ctx = 1024)"}}"#
                                .to_string(),
                        )
                    } else {
                        (404, "{}".to_string())
                    };
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });
        Self { base_url, models }
    }

    pub fn serve(&self, models: serde_json::Value) {
        *self.models.lock().unwrap() = models.to_string();
    }

    pub fn config(&self, name: &str) -> ModelConfig {
        ModelConfig {
            provider: "ollama".into(),
            name: name.into(),
            base_url: Some(self.base_url.clone()),
            ..ModelConfig::default()
        }
    }
}

pub fn list(entries: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({"object": "list", "data": entries})
}
