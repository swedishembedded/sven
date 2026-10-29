// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec-level tests for discovery: parsing tolerance, model choice, change
//! detection, locality, zero-config probing and the time bound.

use super::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn model(id: &str, ctx: Option<u32>) -> ServedModel {
    ServedModel {
        id: id.into(),
        context_length: ctx,
        ..ServedModel::default()
    }
}

fn adapter(id: &str, digest: &str) -> Option<AdapterRef> {
    Some(AdapterRef {
        id: id.into(),
        digest: digest.into(),
    })
}

/// Serve `body` as the answer to every request, forever, on a loopback port.
async fn serve(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    url
}

/// A loopback address that accepts connections and never answers.
async fn silent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    url
}

/// A loopback address nothing listens on.
async fn closed() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    format!("http://{}/v1", listener.local_addr().unwrap())
}

fn candidate(driver: &str, url: &str) -> (String, String, Option<String>) {
    (driver.into(), url.into(), None)
}

#[test]
fn an_old_server_listing_only_ids_parses() {
    let models = parse_served_models(&json!({
        "object": "list",
        "data": [{"id": "a", "object": "model", "created": 1, "owned_by": "x"}, {"id": "b"}]
    }))
    .unwrap();
    assert_eq!(models, vec![model("a", None), model("b", None)]);
}

#[test]
fn a_new_server_reports_adapter_output_cap_and_tool_support() {
    let models = parse_served_models(&json!({"data": [{
        "id": "m",
        "context_length": 8192,
        "max_output_tokens": 2048,
        "capabilities": ["chat", "tools"],
        "adapter": {"id": "lora-1", "digest": "sha256:abc"},
        "something_new": true
    }]}))
    .unwrap();
    let m = &models[0];
    assert_eq!(m.context_length, Some(8192));
    assert_eq!(m.max_output_tokens, Some(2048));
    assert_eq!(m.tools, Some(true));
    assert_eq!(m.adapter, adapter("lora-1", "sha256:abc"));
}

#[test]
fn tool_support_is_unknown_without_a_capability_list_and_false_with_one_lacking_it() {
    let models = parse_served_models(&json!({"data": [
        {"id": "silent"},
        {"id": "plain", "capabilities": ["chat"]}
    ]}))
    .unwrap();
    assert_eq!(models[0].tools, None);
    assert_eq!(models[1].tools, Some(false));
}

#[test]
fn vllm_context_field_is_understood() {
    let models = parse_served_models(&json!({"data": [{"id": "v", "max_model_len": 32768}]}));
    assert_eq!(models.unwrap()[0].context_length, Some(32768));
}

#[test]
fn a_body_that_is_not_a_model_list_is_not_a_server_answer() {
    assert!(parse_served_models(&json!({"error": "nope"})).is_none());
}

#[test]
fn the_first_chat_capable_model_is_chosen_and_non_chat_models_are_skipped() {
    let models = vec![
        model("imageops", None),
        model("chat-a", Some(2048)),
        model("chat-b", Some(4096)),
    ];
    assert_eq!(choose_model(&models).unwrap().id, "chat-a");
}

/// An agent calls tools: a model the server says can call them is chosen
/// over one it says cannot, and over one it says nothing about.
#[test]
fn a_model_that_can_call_tools_is_chosen_over_one_that_cannot() {
    let tools = |id: &str, t: Option<bool>| ServedModel {
        tools: t,
        ..model(id, Some(4096))
    };
    let models = vec![
        tools("no-tools", Some(false)),
        tools("unknown", None),
        tools("agent", Some(true)),
    ];
    assert_eq!(choose_model(&models).unwrap().id, "agent");
    assert_eq!(choose_model(&models[..2]).unwrap().id, "unknown");
    assert_eq!(
        choose_model(&models[..1]).unwrap().id,
        "no-tools",
        "one that cannot is still better than none"
    );
}

/// OpenRouter says which request parameters a model honours; `tools` among
/// them is tool support.
#[test]
fn supported_parameters_name_tool_support() {
    let models = parse_served_models(&json!({"data": [
        {"id": "a", "context_length": 8192, "supported_parameters": ["tools", "temperature"]},
        {"id": "b", "context_length": 8192, "supported_parameters": ["temperature"]},
    ]}))
    .unwrap();
    assert_eq!(models[0].tools, Some(true));
    assert_eq!(models[1].tools, Some(false));
}

#[test]
fn without_any_context_lengths_the_first_listed_model_is_chosen() {
    let models = vec![model("x", None), model("y", None)];
    assert_eq!(choose_model(&models).unwrap().id, "x");
    assert!(choose_model(&[]).is_none());
}

#[test]
fn a_changed_adapter_digest_is_a_change_and_an_unchanged_one_is_not() {
    let old = adapter("a", "d1");
    assert!(adapter_change("m", &old, &adapter("a", "d1")).is_none());
    let change = adapter_change("m", &old, &adapter("a", "d2")).unwrap();
    assert_eq!(change.model, "m");
    assert_eq!(change.to, adapter("a", "d2"));
    assert!(adapter_change("m", &None, &old).is_some());
    assert!(adapter_change("m", &old, &None).is_some());
}

#[test]
fn context_overflow_errors_are_recognised() {
    assert!(is_context_overflow(
        "openai error 400: {\"error\":{\"code\":\"context_length_exceeded\"}}"
    ));
    assert!(is_context_overflow(
        "brain error 400: request exceeds the available context size (n_ctx = 4096)"
    ));
    assert!(is_context_overflow(
        "This model's maximum context length is 8192 tokens"
    ));
    assert!(!is_context_overflow("openai error 401: invalid api key"));
    assert!(!is_context_overflow("model not found"));
}

#[test]
fn only_this_host_and_private_networks_are_local() {
    for local in [
        "http://127.0.0.1:8788/v1",
        "http://localhost:11434/v1",
        "http://[::1]:8000/v1",
        "http://192.168.1.20:1234/v1",
    ] {
        assert!(is_local_endpoint(local), "{local}");
    }
    for remote in [
        "https://api.openai.com/v1",
        "https://8.8.8.8/v1",
        "not a url",
    ] {
        assert!(!is_local_endpoint(remote), "{remote}");
    }
}

#[test]
fn a_probe_is_trusted_for_the_ttl_and_stale_after() {
    let live = LiveState::new("m", true);
    let now = Instant::now();
    assert!(live.is_stale(now), "never probed");
    *live.last_probe.lock().unwrap() = Some(now);
    assert!(!live.is_stale(now + REFRESH_TTL - Duration::from_secs(1)));
    assert!(live.is_stale(now + REFRESH_TTL));
}

#[tokio::test]
async fn zero_config_picks_the_first_responder_in_priority_order() {
    let body = |id: &str| json!({"data": [{"id": id}]}).to_string();
    let (second, third) = (serve(body("second")).await, serve(body("third")).await);
    let found = probe_endpoints(
        vec![
            candidate("brain", &closed().await),
            candidate("ollama", &second),
            candidate("vllm", &third),
        ],
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(found.driver, "ollama");
    assert_eq!(found.models[0].id, "second");
}

#[tokio::test]
async fn zero_config_ignores_a_server_that_serves_nothing() {
    let empty = serve(json!({"data": []}).to_string()).await;
    let found = probe_endpoints(vec![candidate("ollama", &empty)], Duration::from_secs(1)).await;
    assert!(found.is_none());
}

#[tokio::test]
async fn an_unresponsive_candidate_costs_one_timeout_not_the_wait_for_it() {
    let hung = silent().await;
    let live = serve(json!({"data": [{"id": "m"}]}).to_string()).await;
    let started = Instant::now();
    let found = probe_endpoints(
        vec![candidate("brain", &hung), candidate("ollama", &live)],
        Duration::from_millis(300),
    )
    .await
    .unwrap();
    assert_eq!(found.driver, "ollama");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn fetching_from_a_silent_server_gives_up_at_the_timeout() {
    let hung = silent().await;
    let started = Instant::now();
    let got = fetch_served_models(models_request(&hung, None), Duration::from_millis(200)).await;
    assert!(got.is_none());
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_configured_name_is_never_replaced() {
    let url = serve(json!({"data": [{"id": "other"}]}).to_string()).await;
    let cfg = ModelConfig {
        provider: "ollama".into(),
        name: "mine".into(),
        base_url: Some(url),
        ..ModelConfig::default()
    };
    assert_eq!(resolve_empty_model_name(&cfg).await.name, "mine");
}

#[tokio::test]
async fn an_unreachable_server_leaves_the_name_empty() {
    let cfg = ModelConfig {
        provider: "ollama".into(),
        name: String::new(),
        base_url: Some(closed().await),
        ..ModelConfig::default()
    };
    assert_eq!(resolve_empty_model_name(&cfg).await.name, "");
}

#[tokio::test]
async fn a_configuration_that_selects_a_model_is_not_zero_config() {
    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "x".into(),
        ..ModelConfig::default()
    };
    assert_eq!(resolve_unconfigured(&cfg).await.provider, "anthropic");
}

/// A server whose answer to `GET /models` the test changes between probes.
struct Swappable(Mutex<ServedModels>);

impl Swappable {
    fn serving(model: ServedModel) -> Self {
        Self(Mutex::new(vec![model]))
    }
    fn swap(&self, model: ServedModel) {
        *self.0.lock().unwrap() = vec![model];
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl ModelProvider for Swappable {
    fn name(&self) -> &str {
        "swappable"
    }
    fn model_name(&self) -> &str {
        "m"
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<sven_model::ResponseStream> {
        unreachable!("discovery never completes")
    }
    async fn discover(&self) -> Option<ServedModels> {
        Some(self.0.lock().unwrap().clone())
    }
}

fn with_adapter(digest: &str) -> ServedModel {
    ServedModel {
        adapter: adapter("a", digest),
        context_length: Some(8192),
        ..model("m", None)
    }
}

#[tokio::test]
async fn a_swapped_adapter_is_reported_once_and_the_first_probe_is_only_a_baseline() {
    let server = Swappable::serving(with_adapter("d1"));
    let live = LiveState::new("m", true);

    live.refresh(&server).await;
    assert_eq!(live.take_change(), None, "the first answer is a baseline");

    live.refresh(&server).await;
    assert_eq!(live.take_change(), None, "an unchanged adapter is not news");

    server.swap(with_adapter("d2"));
    live.refresh(&server).await;
    assert_eq!(
        live.take_change(),
        Some(ModelChange {
            model: "m".into(),
            from: adapter("a", "d1"),
            to: adapter("a", "d2"),
            context_window: Some(8192),
        })
    );
    assert_eq!(live.take_change(), None, "a change is taken once");
}

#[tokio::test]
async fn swaps_before_the_session_looks_coalesce_and_a_swap_back_cancels_out() {
    let server = Swappable::serving(with_adapter("d1"));
    let live = LiveState::new("m", true);
    live.refresh(&server).await;

    for digest in ["d2", "d3"] {
        server.swap(with_adapter(digest));
        live.refresh(&server).await;
    }
    let change = live.take_change().expect("a change");
    assert_eq!(
        (change.from, change.to),
        (adapter("a", "d1"), adapter("a", "d3"))
    );

    server.swap(with_adapter("d4"));
    live.refresh(&server).await;
    server.swap(with_adapter("d3"));
    live.refresh(&server).await;
    assert_eq!(
        live.take_change(),
        None,
        "back where the session last heard of"
    );
}

#[tokio::test]
async fn the_servers_output_cap_and_tool_support_are_remembered_when_it_reports_them() {
    let server = Swappable::serving(ServedModel {
        max_output_tokens: Some(2048),
        tools: Some(false),
        ..model("m", Some(4096))
    });
    let live = LiveState::new("m", true);
    assert_eq!(
        (live.max_output_tokens(), live.tool_support()),
        (None, None)
    );
    live.refresh(&server).await;
    assert_eq!(live.max_output_tokens(), Some(2048));
    assert_eq!(live.tool_support(), Some(false));
}

#[tokio::test]
async fn the_input_modalities_a_server_reports_decide_whether_images_are_sent() {
    use sven_model::catalog::InputModality::{Image, Text};

    let vision = Swappable::serving(ServedModel {
        input_modalities: Some(vec![Text, Image]),
        ..model("m", Some(4096))
    });
    let live = LiveState::new("m", true);
    live.refresh(&vision).await;
    assert_eq!(live.input_modalities(), Some(vec![Text, Image]));
    let provider = bounded(vision, None, live);
    assert!(
        provider.supports_images(),
        "a local server that serves a vision model is sent images"
    );

    let silent = LiveState::new("m", true);
    silent
        .refresh(&Swappable::serving(model("m", Some(4096))))
        .await;
    assert_eq!(silent.input_modalities(), None, "not reported is unknown");
}

fn bounded(
    server: Swappable,
    configured_cap: Option<u32>,
    live: Arc<LiveState>,
) -> crate::ConfigBoundedProvider {
    crate::ConfigBoundedProvider {
        inner: Arc::new(server),
        config_context_window: None,
        max_output_tokens: configured_cap,
        input_modalities: None,
        live: Some(live),
    }
}

#[tokio::test]
async fn the_servers_output_cap_applies_unless_the_configuration_sets_one() {
    let capped = || ServedModel {
        max_output_tokens: Some(2048),
        ..model("m", Some(4096))
    };
    let live = LiveState::new("m", true);
    let unset = bounded(Swappable::serving(capped()), None, Arc::clone(&live));
    live.refresh(&*unset.inner).await;
    assert_eq!(unset.catalog_max_output_tokens(), Some(2048));
    assert_eq!(unset.tool_support(), None, "silence is not a no");

    let set = bounded(Swappable::serving(capped()), Some(512), live);
    assert_eq!(set.catalog_max_output_tokens(), Some(512));
}

fn signed_in_start(base_url: String, fallback: Option<ModelConfig>) -> ModelConfig {
    ModelConfig {
        provider: "sven".into(),
        name: String::new(),
        base_url: Some(base_url),
        api_key: Some("sk-signed-in".into()),
        detection_fallback: fallback.map(Box::new),
        ..ModelConfig::default()
    }
}

/// A signed-in start uses the default model the account names, whatever
/// the list's order.
#[tokio::test]
async fn a_signed_in_start_uses_the_accounts_default_model() {
    let url = serve(
        json!({"data": [{"id": "first", "context_length": 8192}, {"id": "deepseek/deepseek-v4.1-flash"}],
               "default_model": "deepseek/deepseek-v4.1-flash"})
        .to_string(),
    )
    .await;
    let cfg = resolve_sven_default(&signed_in_start(url, None)).await;
    assert_eq!(cfg.name, "deepseek/deepseek-v4.1-flash");
    assert!(cfg.detection_fallback.is_none());
}

/// A server that names no default (or one it does not list) leaves the
/// choice to the list, as for any server.
#[tokio::test]
async fn without_a_listed_default_the_list_chooses() {
    let url = serve(
        json!({"data": [{"id": "chat", "context_length": 8192}], "default_model": "gone"})
            .to_string(),
    )
    .await;
    assert_eq!(
        resolve_sven_default(&signed_in_start(url, None)).await.name,
        "chat"
    );
}

/// A server that does not answer yields to the cloud choice held in reserve.
#[tokio::test]
async fn an_unreachable_server_yields_to_the_reserve() {
    let reserve = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-sonnet-4-6".into(),
        ..ModelConfig::default()
    };
    let cfg = resolve_sven_default(&signed_in_start(closed().await, Some(reserve))).await;
    assert_eq!(
        (cfg.provider.as_str(), cfg.name.as_str()),
        ("anthropic", "claude-sonnet-4-6")
    );
}

#[tokio::test]
async fn a_probed_client_of_a_local_vision_model_accepts_images() {
    let url = serve(
        json!({"data": [{"id": "vl", "architecture": {"input_modalities": ["text", "image"]}}]})
            .to_string(),
    )
    .await;
    let cfg = ModelConfig {
        provider: "vllm".into(),
        name: "vl".into(),
        base_url: Some(url),
        ..ModelConfig::default()
    };
    let client = crate::from_config_probed(&cfg).await.unwrap();
    assert!(client.supports_images(), "{:?}", client.input_modalities());
}
