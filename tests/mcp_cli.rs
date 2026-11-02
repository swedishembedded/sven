// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that are given new tools
// in one command for its clients. If your team needs expertise in Model
// Context Protocol integration then you can procure our services by sending an
// email to info@swedishembedded.com.
//! `sven mcp add`, `list` and `remove`, end to end: sven adds itself (`sven
//! mcp serve`) as a stdio MCP server, finds it connects with its tools, and
//! removes it again, in a home of its own; and a headless run waits for a
//! slow server's tools before its model's first step.

#![cfg(feature = "network")]

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("sven-mcp-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["home", "config", "run", "proj"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        Self(dir)
    }

    fn sven(&self, args: &[&str]) -> (bool, String) {
        self.sven_with(args, &[])
    }

    fn sven_with(&self, args: &[&str], env: &[(&str, &str)]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_sven"))
            .current_dir(self.0.join("proj"))
            .env_clear()
            .env("HOME", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_RUNTIME_DIR", self.0.join("run"))
            .env("PATH", "/usr/bin:/bin")
            .envs(env.iter().copied())
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("the sven binary runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_server_is_added_checked_listed_and_removed() {
    let home = Home::new("add");
    let sven = env!("CARGO_BIN_EXE_sven");

    let (ok, text) = home.sven(&["mcp", "list"]);
    assert!(
        ok && text.contains("No MCP servers are configured"),
        "{text}"
    );

    let (ok, text) = home.sven(&["mcp", "add", "self", "--", sven, "mcp", "serve"]);
    assert!(ok, "{text}");
    assert!(text.contains("Added MCP server 'self'"), "{text}");
    assert!(text.contains("It connects:"), "{text}");

    let file = home.0.join("config/sven/config.yaml");
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(
        written.contains("mcp_servers") && written.contains("serve"),
        "{written}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let (ok, text) = home.sven(&["mcp", "add", "self", "--", sven, "mcp", "serve"]);
    assert!(
        !ok && text.contains("already has an MCP server 'self'"),
        "{text}"
    );

    let (ok, text) = home.sven(&["mcp", "list"]);
    assert!(ok, "{text}");
    let line = text
        .lines()
        .find(|l| l.starts_with("self"))
        .expect("listed");
    assert!(
        line.contains("connected") && line.contains("tools"),
        "{line}"
    );

    let (ok, text) = home.sven(&["mcp", "remove", "self"]);
    assert!(ok && text.contains("Removed MCP server 'self'"), "{text}");
    let (ok, text) = home.sven(&["mcp", "list"]);
    assert!(
        ok && text.contains("No MCP servers are configured"),
        "{text}"
    );
}

#[test]
fn an_entry_that_is_not_a_server_is_refused_before_anything_is_written() {
    let home = Home::new("refuse");
    let (ok, text) = home.sven(&[
        "mcp",
        "add",
        "web",
        "https://mcp.example.com/mcp",
        "--env",
        "A=b",
    ]);
    assert!(
        !ok && text.contains("--env is for a stdio server"),
        "{text}"
    );
    let (ok, text) = home.sven(&["mcp", "add", "bad name", "https://mcp.example.com/mcp"]);
    assert!(!ok && text.contains("letters, digits"), "{text}");
    assert!(!home.0.join("config/sven/config.yaml").exists());
}

#[test]
fn a_headless_run_waits_for_a_slow_servers_tools() {
    let home = Home::new("headless");
    let sven = env!("CARGO_BIN_EXE_sven");
    let serve = format!("sleep 2; exec '{sven}' mcp serve");
    let (ok, text) = home.sven(&["mcp", "add", "slow", "--", "sh", "-c", &serve]);
    assert!(ok, "{text}");

    std::fs::write(home.0.join("proj/note.txt"), "written before the run").unwrap();
    let log = home.0.join("requests.jsonl");
    let mock = home.0.join("mock.yaml");
    std::fs::write(
        &mock,
        format!(
            "request_log: {}\nresponses:\n  - match_type: default\n    tool_calls:\n      - id: tc-read\n        tool: slow-read_file\n        args: {{ path: note.txt }}\n    after_tool_reply: \"Done.\"\n",
            log.display()
        ),
    )
    .unwrap();
    // The model answers at once, while the server is still starting: the
    // run must hold its first step until the server's tools are there.
    let (_, text) = home.sven_with(
        &["--headless", "--model", "mock", "read the note"],
        &[("SVEN_MOCK_RESPONSES", mock.to_str().unwrap())],
    );
    let requests = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        requests.contains("written before the run"),
        "the slow server's tool never ran: {text}"
    );
}
