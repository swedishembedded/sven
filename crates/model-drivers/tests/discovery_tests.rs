// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for local model serving for its
// clients. If your team needs expertise in self-hosted LLM infrastructure then
// you can procure our services by sending an email to info@swedishembedded.com.
//! Discovery through the public factory: a local server says what it serves,
//! and sven follows it without any model name in the configuration.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sven_model::{CompletionRequest, Message};
use sven_model_drivers::{from_config_probed, ModelConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A loopback model server whose `/models` answer can be changed while it
/// runs. `/props` is absent (404) and chat completions are rejected with a
/// context-overflow error, like a server whose window is smaller than the
/// prompt.
struct Server {
    base_url: String,
    models: Arc<Mutex<String>>,
}

impl Server {
    async fn start(models: serde_json::Value) -> Self {
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

    fn serve(&self, models: serde_json::Value) {
        *self.models.lock().unwrap() = models.to_string();
    }

    fn config(&self, name: &str) -> ModelConfig {
        ModelConfig {
            provider: "ollama".into(),
            name: name.into(),
            base_url: Some(self.base_url.clone()),
            ..ModelConfig::default()
        }
    }
}

fn list(entries: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({"object": "list", "data": entries})
}

#[tokio::test]
async fn an_empty_name_resolves_to_the_single_served_model() {
    let server = Server::start(list(vec![serde_json::json!({"id": "only"})])).await;
    let provider = from_config_probed(&server.config("")).await.unwrap();
    assert_eq!(provider.model_name(), "only");
}

#[tokio::test]
async fn an_empty_name_with_several_models_picks_the_first_chat_capable_one() {
    let server = Server::start(list(vec![
        serde_json::json!({"id": "imageops"}),
        serde_json::json!({"id": "chat-a", "context_length": 2048}),
        serde_json::json!({"id": "chat-b", "context_length": 4096}),
    ]))
    .await;
    let provider = from_config_probed(&server.config("")).await.unwrap();
    assert_eq!(provider.model_name(), "chat-a");
    assert_eq!(provider.catalog_context_window(), Some(2048));
}

#[tokio::test]
async fn a_configured_name_always_wins() {
    let server = Server::start(list(vec![serde_json::json!({"id": "other"})])).await;
    let provider = from_config_probed(&server.config("mine")).await.unwrap();
    assert_eq!(provider.model_name(), "mine");
}

#[tokio::test]
async fn an_old_server_without_the_new_fields_works() {
    let server = Server::start(list(vec![serde_json::json!({
        "id": "m", "object": "model", "created": 1, "owned_by": "x"
    })]))
    .await;
    let provider = from_config_probed(&server.config("")).await.unwrap();
    let served = provider.discover().await.unwrap();
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].adapter, None);
    assert_eq!(served[0].tools, None);
    assert_eq!(served[0].context_length, None);
}

#[tokio::test]
async fn a_new_server_reports_adapter_output_cap_and_tools() {
    let server = Server::start(list(vec![serde_json::json!({
        "id": "m", "context_length": 8192, "max_output_tokens": 1024,
        "capabilities": ["chat", "tools"],
        "adapter": {"id": "lora", "digest": "d1"}
    })]))
    .await;
    let provider = from_config_probed(&server.config("m")).await.unwrap();
    let served = provider.discover().await.unwrap();
    assert_eq!(served[0].max_output_tokens, Some(1024));
    assert_eq!(served[0].tools, Some(true));
    assert_eq!(served[0].adapter.as_ref().unwrap().digest, "d1");
}

#[tokio::test]
async fn a_hot_swapped_adapter_updates_the_context_window_on_re_probe() {
    let entry = |ctx: u32, digest: &str| {
        list(vec![serde_json::json!({
            "id": "m", "context_length": ctx, "adapter": {"id": "lora", "digest": digest}
        })])
    };
    let server = Server::start(entry(2048, "d1")).await;
    let provider = from_config_probed(&server.config("m")).await.unwrap();
    assert_eq!(provider.catalog_context_window(), Some(2048));

    server.serve(entry(8192, "d2"));
    let served = provider.discover().await.unwrap();
    assert_eq!(served[0].adapter.as_ref().unwrap().digest, "d2");
    assert_eq!(provider.catalog_context_window(), Some(8192));
}

#[tokio::test]
async fn a_context_overflow_error_re_probes_before_it_is_returned() {
    let entry = |ctx: u32| list(vec![serde_json::json!({"id": "m", "context_length": ctx})]);
    let server = Server::start(entry(4096)).await;
    let provider = from_config_probed(&server.config("m")).await.unwrap();
    assert_eq!(provider.catalog_context_window(), Some(4096));

    server.serve(entry(1024));
    let req = CompletionRequest {
        messages: vec![Message::user("hi")],
        ..CompletionRequest::default()
    };
    assert!(provider.complete(req).await.is_err());
    assert_eq!(provider.catalog_context_window(), Some(1024));
}

#[tokio::test]
async fn an_unreachable_server_neither_fails_nor_blocks_the_build() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cfg = ModelConfig {
        provider: "ollama".into(),
        name: String::new(),
        base_url: Some(format!("http://{}/v1", closed.local_addr().unwrap())),
        ..ModelConfig::default()
    };
    drop(closed);
    let started = Instant::now();
    let provider = from_config_probed(&cfg).await.unwrap();
    assert_eq!(provider.model_name(), "");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// The configuration brain's local auto-detection produces when a cloud key
/// is also present: brain first, with the cloud choice held in reserve.
fn detected_brain(base_url: String, fallback: ModelConfig) -> ModelConfig {
    ModelConfig {
        provider: "brain".into(),
        name: String::new(),
        base_url: Some(base_url),
        api_key: Some("sk-brain-test".into()),
        detection_fallback: Some(Box::new(fallback)),
        ..ModelConfig::default()
    }
}

fn cloud_choice() -> ModelConfig {
    ModelConfig {
        provider: "ollama".into(),
        name: "cloud-choice".into(),
        base_url: Some("http://127.0.0.1:9/v1".into()),
        ..ModelConfig::default()
    }
}

#[tokio::test]
async fn a_detected_brain_that_does_not_answer_yields_to_the_cloud_choice() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_url = format!("http://{}/v1", closed.local_addr().unwrap());
    drop(closed);
    let provider = from_config_probed(&detected_brain(dead_url, cloud_choice()))
        .await
        .unwrap();
    assert_eq!(provider.model_name(), "cloud-choice");
}

#[tokio::test]
async fn a_detected_brain_that_serves_no_model_yields_to_the_cloud_choice() {
    let server = Server::start(list(vec![])).await;
    let provider = from_config_probed(&detected_brain(server.base_url.clone(), cloud_choice()))
        .await
        .unwrap();
    assert_eq!(provider.model_name(), "cloud-choice");
}

#[tokio::test]
async fn a_detected_brain_that_answers_is_kept_over_the_cloud_choice() {
    let server = Server::start(list(vec![serde_json::json!({"id": "resident"})])).await;
    let provider = from_config_probed(&detected_brain(server.base_url.clone(), cloud_choice()))
        .await
        .unwrap();
    assert_eq!(provider.model_name(), "resident");
}
