// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared by the production-run specs: a loopback chat-completions server that
//! plays a script, a disposable copy of the sample repository, and the helpers
//! that run the real binary against them. Each spec file uses a different part,
//! so unused items are expected here.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in letting agents change real
//! repositories under verifiable control then you can procure our services
//! by sending an email to info@swedishembedded.com.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

pub const KEY: &str = "sk-test-5f0c9d7e-not-a-real-key";
pub const MODEL: &str = "openai/scripted-coder";

pub fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/harness/c-repair")
}

/// What the model server saw of the run.
#[derive(Default)]
pub struct Seen {
    pub authorization: Vec<String>,
    pub requests: usize,
}

/// A loopback chat-completions server that answers its n-th request with the
/// n-th scripted turn: a list of tool calls `(name, arguments)`, or a text.
pub struct Server {
    pub base: String,
    pub seen: Arc<Mutex<Seen>>,
}

pub enum Turn {
    /// Holds the connection open without answering until the test ends.
    Stall,
    Calls(Vec<(&'static str, Value)>),
    Text(&'static str),
}

pub fn serve(script: Vec<Turn>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let next = Arc::new(AtomicUsize::new(0));
    let script = Arc::new(script);
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (script, next, log) = (Arc::clone(&script), Arc::clone(&next), Arc::clone(&log));
            std::thread::spawn(move || answer(stream, &script, &next, &log));
        }
    });
    Server { base, seen }
}

fn answer(mut stream: TcpStream, script: &[Turn], next: &AtomicUsize, log: &Mutex<Seen>) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_string();
    let length = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    let _ = stream.read_exact(&mut body);
    if !head.starts_with("POST /v1/chat/completions") {
        let _ =
            stream.write_all(b"HTTP/1.1 404 X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        return;
    }
    {
        let mut seen = log.lock().unwrap();
        seen.requests += 1;
        if let Some(auth) = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
        {
            seen.authorization
                .push(auth.split_once(':').unwrap().1.trim().to_string());
        }
    }
    let n = next.fetch_add(1, Ordering::SeqCst);
    if matches!(script.get(n), Some(Turn::Stall)) {
        std::thread::sleep(std::time::Duration::from_secs(120));
        return;
    }
    let mut events = Vec::new();
    match script.get(n) {
        Some(Turn::Calls(calls)) => {
            for (index, (name, args)) in calls.iter().enumerate() {
                events.push(json!({"choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": index, "id": format!("call-{n}-{index}"), "type": "function",
                    "function": {"name": name, "arguments": args.to_string()}}]}}]}));
            }
            events.push(
                json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
            );
        }
        other => {
            let text = match other {
                Some(Turn::Text(text)) => (*text).to_string(),
                _ => "the script has no more turns".to_string(),
            };
            events.push(json!({"choices": [{"index": 0, "delta": {"content": text}}]}));
            events.push(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}));
        }
    }
    let mut reply = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    );
    for event in events {
        reply.push_str(&format!("data: {event}\n\n"));
    }
    reply.push_str("data: [DONE]\n\n");
    let _ = stream.write_all(reply.as_bytes());
}

/// A fresh copy of the sample's repository in a tempdir, and a scratch
/// directory outside it that holds the key file and the run's output files.
pub struct Setup {
    pub repo: tempfile::TempDir,
    pub scratch: tempfile::TempDir,
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

pub fn setup() -> Setup {
    let repo = tempfile::tempdir().unwrap();
    copy_dir(&sample().join("repo"), repo.path());
    Setup {
        repo,
        scratch: tempfile::tempdir().unwrap(),
    }
}

impl Setup {
    pub fn key_file(&self) -> PathBuf {
        let file = self.scratch.path().join("keys.json");
        std::fs::write(&file, json!({"openai": KEY}).to_string()).unwrap();
        file
    }

    pub fn out(&self, name: &str) -> PathBuf {
        self.scratch.path().join(name)
    }

    /// `sven harness run` of the sample as a coding run on the repo.
    pub fn command(&self, server: &Server) -> Command {
        self.subcommand("run", &sample(), server)
    }

    /// `sven harness <subcommand> <harness>` as a coding run on the repo.
    pub fn subcommand(&self, subcommand: &str, harness: &Path, server: &Server) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sven"));
        cmd.current_dir(self.scratch.path())
            .env("HOME", self.scratch.path())
            .env("XDG_CONFIG_HOME", self.scratch.path())
            .env_remove("SVEN_MODEL")
            .args(["harness", subcommand])
            .arg(harness)
            .args(["--toolset", "coding", "--workspace"])
            .arg(self.repo.path())
            .args([
                "--model",
                MODEL,
                "--model-endpoint",
                &server.base,
                "--api-key-file",
            ])
            .arg(self.key_file())
            .args(["--json", "--timeout", "120"]);
        cmd
    }
}

pub fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("the sven binary runs")
}

pub fn code(out: &Output) -> i32 {
    out.status.code().expect("exited normally")
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

pub fn report(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is pure JSON ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            stderr(out)
        )
    })
}

pub fn write(path: &str, text: &str) -> (&'static str, Value) {
    ("write_file", json!({"path": path, "text": text}))
}

pub fn shell(command: &str) -> (&'static str, Value) {
    ("shell", json!({"shell_command": command}))
}
