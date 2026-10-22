// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The real `sven` binary with no configuration and nothing in its
//! environment but a home and a runtime directory.
//!
//! The specs: a brain server running here is used with no configuration,
//! from the connection record it publishes (its address and key); a record
//! whose process is gone is not; with nothing to use, the error says what to
//! run, never what to export; a key stored with `sven auth set` is private
//! and never sent to another endpoint, while a key in the environment serves
//! nothing; `doctor provider` reports a refused key and an unserved model;
//! `sven list-models` lists what can be used now under the names `--model`
//! takes.
//!
//! Swedish Embedded AB implements solutions for agents that start working
//! with no setup on a machine that already serves models, for its clients. If
//! your team needs expertise in zero-configuration local and hosted LLM use
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use serde_json::{json, Value};

/// A loopback OpenAI-shaped server whose `/v1/models` answers `models` to a
/// request bearing `key`, and 401 to any other. Returns its origin
/// (`http://127.0.0.1:PORT`). Lives until the test ends.
fn serve(models: Value, key: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let body = models.to_string();
    let auth = format!("authorization: bearer {}", key.to_ascii_lowercase());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (body, auth) = (body.clone(), auth.clone());
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                let (status, body) = if !head.contains(&auth) {
                    (
                        401,
                        r#"{"error":{"message":"missing or invalid API key"}}"#.to_string(),
                    )
                } else if head.starts_with("get /v1/models") {
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
    origin
}

fn models() -> Value {
    json!({"object": "list", "data": [
        {"id": "Qwen/Qwen3-0.6B", "context_length": 40960, "capabilities": ["chat", "tools"]},
    ]})
}

/// A home, config and runtime directory of one test, removed when it ends.
struct Machine(PathBuf);

impl Machine {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sven-zero-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["home", "config", "run/brain", "work"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::set_permissions(
            dir.join("run/brain"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        Self(dir)
    }

    /// Publish a brain connection record for `pid` serving at `origin`.
    fn brain_record(&self, pid: u32, origin: &str, key: &str) {
        let record = self.0.join(format!("run/brain/serve-{pid}.json"));
        let body =
            json!({"pid": pid, "surfaces": [{"provider": "openai", "url": origin, "key": key}]});
        std::fs::write(&record, body.to_string()).unwrap();
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn write_config(&self, yaml: &str) {
        std::fs::create_dir_all(self.0.join("config/sven")).unwrap();
        std::fs::write(self.0.join("config/sven/config.yaml"), yaml).unwrap();
    }

    /// `sven <args>` with nothing in its environment but this machine.
    fn sven(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sven"));
        cmd.current_dir(self.0.join("work"))
            .env_clear()
            .env("HOME", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_RUNTIME_DIR", self.0.join("run"))
            .env("PATH", "/usr/bin:/bin")
            .args(args)
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.sven(args).output().expect("the sven binary runs")
    }
}

impl Drop for Machine {
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

fn check<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no check {name} in {report}"))
}

fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
fn a_running_brain_is_used_with_no_configuration() {
    let machine = Machine::new("brain");
    let origin = serve(models(), "sk-brain-record");
    machine.brain_record(std::process::id(), &origin, "sk-brain-record");
    let out = machine.run(&["doctor", "provider", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let checked = report(&out);
    assert_eq!(
        check(&checked, "provider")["detail"],
        "brain/Qwen/Qwen3-0.6B",
        "{checked}"
    );
    assert_eq!(check(&checked, "endpoint")["verdict"], "ok");
    assert_eq!(check(&checked, "tools")["verdict"], "ok");

    let out = machine.run(&["list-models", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let refs: Vec<String> = report(&out)["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["reference"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        refs,
        ["brain/Qwen/Qwen3-0.6B"],
        "the name --model takes for it"
    );
}

#[test]
fn with_nothing_to_use_the_error_says_what_to_run_not_what_to_export() {
    let machine = Machine::new("nothing");
    let origin = serve(models(), "sk-brain-gone");
    machine.brain_record(dead_pid(), &origin, "sk-brain-gone");
    let out = machine.run(&["doctor", "provider"]);
    assert_ne!(
        out.status.code(),
        Some(0),
        "a record whose process is gone names no server"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("sven auth set"), "{text}");
    assert!(!text.contains("export"), "keys are not exported: {text}");
}

/// `sven auth set` stores a key once, owner-only and never echoed; the
/// environment's variables name no key; and a stored key never follows a
/// `base_url` to another endpoint, since configuration is also read from the
/// working directory, which a cloned repository controls.
#[test]
fn a_stored_key_is_kept_private_and_never_sent_to_another_endpoint() {
    let machine = Machine::new("stored");
    let origin = serve(models(), "sk-stored-once");
    // An entry for the driver at another endpoint, naming no key of its own.
    machine.write_config(&format!(
        "providers:\n  gateway:\n    name: openrouter\n    base_url: {origin}/v1\n"
    ));
    let gateway = [
        "-M",
        "gateway/Qwen/Qwen3-0.6B",
        "doctor",
        "provider",
        "--json",
    ];

    let out = machine.run(&gateway);
    assert_ne!(out.status.code(), Some(0), "no key yet: {}", report(&out));

    let out = machine
        .sven(&gateway)
        .env("OPENROUTER_API_KEY", "sk-stored-once")
        .output()
        .unwrap();
    assert_ne!(
        out.status.code(),
        Some(0),
        "a key in the environment serves nothing: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let mut set = machine
        .sven(&["auth", "set", "openrouter"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    set.stdin
        .take()
        .unwrap()
        .write_all(b"sk-stored-once\n")
        .unwrap();
    let stored = set.wait_with_output().unwrap();
    assert_eq!(
        stored.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&stored.stderr)
    );
    let file = machine.0.join("config/sven/credentials.yaml");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600,
        "the key is its owner's alone"
    );
    assert!(
        !String::from_utf8_lossy(&stored.stdout).contains("sk-stored-once"),
        "the key is never echoed"
    );
    let out = machine.run(&["auth", "list"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "openrouter");

    let out = machine.run(&gateway);
    assert_ne!(
        out.status.code(),
        Some(0),
        "the stored OpenRouter key is not sent to the gateway's endpoint: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let out = machine.run(&["auth", "remove", "openrouter"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&machine.run(&["auth", "list"]).stdout).trim(),
        "no stored keys; `sven auth set <provider>` stores one"
    );
}

#[test]
fn a_provider_that_refuses_the_key_fails_the_endpoint_check() {
    let machine = Machine::new("refused");
    let origin = serve(models(), "sk-the-right-one");
    machine.write_config(&format!(
        "providers:\n  gateway:\n    name: openrouter\n    base_url: {origin}/v1\n    api_key: sk-a-wrong-one\n"
    ));
    let out = machine.run(&[
        "-M",
        "gateway/Qwen/Qwen3-0.6B",
        "doctor",
        "provider",
        "--json",
    ]);
    assert_ne!(out.status.code(), Some(0));
    let report = report(&out);
    let endpoint = check(&report, "endpoint");
    assert_eq!(endpoint["verdict"], "fail", "{report}");
}

#[test]
fn a_model_its_provider_does_not_serve_is_reported_before_the_first_request() {
    let machine = Machine::new("unserved");
    let origin = serve(models(), "sk-k");
    machine.write_config(&format!("providers:\n  gateway:\n    name: openrouter\n    base_url: {origin}/v1\n    api_key: sk-k\n"));
    let out = machine.run(&[
        "-M",
        "gateway/nonexistent/model-x",
        "doctor",
        "provider",
        "--json",
    ]);
    assert_ne!(out.status.code(), Some(0));
    let report = report(&out);
    let model = check(&report, "model");
    assert_eq!(model["verdict"], "fail");
    assert!(
        model["detail"]
            .as_str()
            .unwrap()
            .contains("Qwen/Qwen3-0.6B"),
        "{model}"
    );
}

/// A configured base URL selects the record of that server, and with it its
/// key; it is never another server's.
#[test]
fn a_brain_named_by_its_base_url_gets_that_servers_key() {
    let machine = Machine::new("named");
    let origin = serve(models(), "sk-brain-named");
    machine.brain_record(std::process::id(), &origin, "sk-brain-named");
    machine.write_config(&format!(
        "model:\n  provider: brain\n  name: Qwen/Qwen3-0.6B\n  base_url: {origin}/v1\n"
    ));
    let out = machine.run(&["doctor", "provider", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
