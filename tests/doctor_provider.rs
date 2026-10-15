// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven doctor provider`: the real binary against a loopback model server.
//!
//! Every test runs with a tempdir as its working directory and a throwaway
//! HOME, no model environment and brain auto-detection off, so only the
//! config file each test writes decides what is checked. The specs: a served
//! model that can call tools is ready; a server that is down, a model it does
//! not serve and a missing key each fail with what to change and a non-zero
//! exit; `--json` is pure JSON.
//!
//! Swedish Embedded AB implements agent setups that explain what is wrong
//! before the first request for its clients. If your team needs expertise in
//! making LLM provider configuration diagnosable then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// A loopback server whose `/v1/models` answers with `models`; anything else is
/// a 404. Lives until the test ends.
fn serve(models: Value) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let body = models.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let body = body.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let (status, body) = if head.starts_with("GET /v1/models") {
                    (200, body)
                } else {
                    (404, "{}".to_string())
                };
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            });
        }
    });
    base
}

/// A base URL nothing listens on.
fn dead() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    drop(listener);
    base
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sven-doctor-provider-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// `sven --config <yaml> doctor provider --json`.
    fn doctor(&self, model_yaml: &str) -> Output {
        let config: &Path = &self.0.join("config.yaml");
        std::fs::write(config, model_yaml).unwrap();
        Command::new(env!("CARGO_BIN_EXE_sven"))
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", &self.0)
            .env("SVEN_DISABLE_BRAIN_AUTODETECT", "1")
            .env_remove("SVEN_MODEL")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
            .args(["--config"])
            .arg(config)
            .args(["doctor", "provider", "--json"])
            .output()
            .expect("the sven binary runs")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn report(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is pure JSON ({e}): {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn verdict(report: &Value, name: &str) -> String {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no check {name} in {report}"))["verdict"]
        .as_str()
        .unwrap()
        .to_string()
}

fn local_model(base: &str, name: &str) -> String {
    format!("model:\n  provider: ollama\n  name: {name}\n  base_url: {base}\n")
}

#[test]
fn a_served_model_that_can_call_tools_is_ready() {
    let base = serve(
        serde_json::json!({"object": "list", "data": [{"id": "resident", "capabilities": ["chat", "tools"]}]}),
    );
    let scratch = Scratch::new("ready");
    let out = scratch.doctor(&local_model(&base, "resident"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = report(&out);
    assert_eq!(verdict(&report, "endpoint"), "ok");
    assert_eq!(verdict(&report, "model"), "ok");
    assert_eq!(verdict(&report, "tools"), "ok");
}

#[test]
fn a_server_that_is_down_fails_the_endpoint_check_and_exits_non_zero() {
    let scratch = Scratch::new("down");
    let out = scratch.doctor(&local_model(&dead(), "m"));
    assert_ne!(out.status.code(), Some(0));
    let report = report(&out);
    assert_eq!(verdict(&report, "endpoint"), "fail");
}

#[test]
fn a_model_the_server_does_not_serve_is_named_with_what_it_does_serve() {
    let base = serve(serde_json::json!({"object": "list", "data": [{"id": "other"}]}));
    let scratch = Scratch::new("unserved");
    let out = scratch.doctor(&local_model(&base, "wanted"));
    assert_ne!(out.status.code(), Some(0));
    let report = report(&out);
    let model = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "model")
        .unwrap();
    assert_eq!(model["verdict"], "fail");
    let detail = model["detail"].as_str().unwrap();
    assert!(
        detail.contains("wanted") && detail.contains("other"),
        "{detail}"
    );
}

#[test]
fn a_key_variable_that_is_not_set_fails_the_provider_check_without_any_network() {
    let scratch = Scratch::new("nokey");
    let out = scratch.doctor("model:\n  provider: anthropic\n  name: claude-sonnet-4-6\n  api_key_env: SVEN_DOCTOR_TEST_NO_SUCH_KEY\n");
    assert_ne!(out.status.code(), Some(0));
    let report = report(&out);
    assert_eq!(verdict(&report, "provider"), "fail");
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["name"] != "endpoint"));
}
