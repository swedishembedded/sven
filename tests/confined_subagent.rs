// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for delegating work to sub-agents
// that stay inside the confinement of their parent, for its clients. If your
// team needs expertise in least-privilege agent delegation then you can
// procure our services by sending an email to info@swedishembedded.com.
//! Spec: a confined session delegates, and the sub-agent is as confined as the
//! commands of its parent. The parent (a scripted model) calls `task`; the
//! child is the real `sven` process, started inside the parent's Landlock
//! confinement and holding the parent's egress policy and its own state root;
//! its model (a loopback server) has it try to escape through its shell, a
//! grandchild of its shell and its web tool. Nothing reaches outside the tree,
//! no `/proc/<pid>` entry of any process (another process's, or its own) is
//! readable, no host outside the egress policy is reachable, and the
//! commands of the child's tools cannot even reach the child's own model.

#![cfg(target_os = "linux")]

mod harness_support;

use std::sync::{Arc, Mutex};

use harness_support::{serve_on, Turn};
use serde_json::json;
use sven_model::ResponseEvent;
use sven_model_mock::CallbackProvider;
use sven_sdk::{Engine, Toolset};

fn shell(command: &str) -> (&'static str, serde_json::Value) {
    ("shell", json!({ "shell_command": command }))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_confined_sessions_sub_agent_and_its_tools_stay_inside_the_confinement() {
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("tree");
    std::fs::create_dir_all(tree.join(".tmp")).unwrap();
    let outside = root.path().join("outside-secret.txt");
    std::fs::write(&outside, "top-secret-outside-the-tree").unwrap();
    let bystander = std::process::Command::new("sleep")
        .arg("120")
        .spawn()
        .unwrap();
    let bystander_pid = bystander.id();
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _bystander = Kill(bystander);

    // The child's model: from its shell it tries to read outside the tree,
    // another process's command line, its own auxv, the same from a
    // grandchild, and to dial a port its own process may use; from its web
    // tool it tries a host the policy does not list; and it does one permitted
    // thing. The marker is spelled differently in a command than in its
    // output, so seeing it in what the model is told means a command succeeded.
    let extra_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let extra = extra_port.local_addr().unwrap().port();
    let escape = |what: &str| format!(": < {what} && echo ESCAPE'D'");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let model_port = listener.local_addr().unwrap().port();
    let model = serve_on(
        listener,
        vec![
            Turn::Calls(vec![
                shell("echo hi > made.txt && echo WROTE-IN-TREE"),
                shell(&escape(&outside.display().to_string())),
                shell(&escape(&format!("/proc/{bystander_pid}/cmdline"))),
                shell(&escape("/proc/self/auxv")),
                shell(&format!(
                    "sh -c \"{}\"",
                    escape(&outside.display().to_string())
                )),
                shell(&format!(
                    "exec 3<>/dev/tcp/127.0.0.1/{extra} && echo ESCAPE'D'"
                )),
                // The child's own model, from its tools' commands and a grandchild.
                shell(&format!(
                    "exec 3<>/dev/tcp/127.0.0.1/{model_port} && echo ESCAPE'D'"
                )),
                shell(&format!(
                    "sh -c \"exec 3<>/dev/tcp/127.0.0.1/{model_port} && echo ESCAPE'D'\""
                )),
                ("tools", json!({ "action": "load", "names": ["web_fetch"] })),
            ]),
            Turn::Calls(vec![(
                "web_fetch",
                json!({ "url": format!("http://127.0.0.1:{extra}/") }),
            )]),
            Turn::Text("done"),
        ],
    );
    // The parent's session: confined commands, the scripted parent, the real
    // `sven` as its sub-agents' executable, and an egress policy that lists
    // the model and one extra port the child's process may therefore dial.
    sven_http::egress::install(
        sven_http::egress::EgressPolicy::from_config(&sven_http::egress::NetworkConfig {
            allow_hosts: Some(vec![
                format!("127.0.0.1:{model_port}"),
                format!("allowed.test:{extra}"),
            ]),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    let mut config = sven_sdk::config::Config::default();
    config.tools.confine = Some(sven_tools_exec::confine::CommandConfinement {
        writable: vec![tree.clone()],
        read_exec: Vec::new(),
        tmpdir: Some(tree.join(".tmp")),
        connect_ports: Vec::new(),
    });
    config.agent.child_executable = Some(env!("CARGO_BIN_EXE_sven").into());
    config.model = sven_model_drivers::ModelConfig {
        provider: "openai".into(),
        name: "scripted-coder".into(),
        base_url: Some(model.base.clone()),
        api_key: Some(harness_support::KEY.into()),
        ..Default::default()
    };
    let calls = Arc::new(Mutex::new(0));
    let provider: Arc<dyn sven_model::ModelProvider> = Arc::new(CallbackProvider::new(move |_| {
        let mut n = calls.lock().unwrap();
        *n += 1;
        if *n == 1 {
            vec![
                ResponseEvent::ToolCall {
                    index: 0,
                    id: "delegate".into(),
                    name: "task".into(),
                    arguments: json!({ "prompt": "probe the confinement", "mode": "agent" })
                        .to_string(),
                },
                ResponseEvent::Done,
            ]
        } else {
            vec![
                ResponseEvent::TextDelta("finished".into()),
                ResponseEvent::Done,
            ]
        }
    }));
    let engine = Engine::builder()
        .config(config)
        .model_provider(provider)
        .project_root(&tree)
        .toolset(Toolset::coding())
        .build()
        .expect("an engine builds");
    engine
        .agent("agent")
        .send("delegate it")
        .await
        .expect("a reply");

    // Nothing dialled the extra port from the child's tools; only the policy
    // refused its web tool, which never connected either.
    extra_port.set_nonblocking(true).unwrap();
    assert!(
        extra_port.accept().is_err(),
        "something connected to the extra port"
    );
    let seen = model.seen.lock().unwrap();
    assert!(
        seen.requests >= 2,
        "the child never finished its turn: {}",
        seen.requests
    );
    let answers = &seen.bodies[1];
    assert!(
        answers.contains("WROTE-IN-TREE"),
        "the permitted write failed: {answers}"
    );
    assert!(
        tree.join("made.txt").is_file(),
        "the child wrote inside the tree"
    );
    assert!(
        !answers.contains("ESCAPED"),
        "the child's tools escaped: {answers}"
    );
    assert!(!answers.contains("top-secret-outside-the-tree"));
    assert!(
        answers.matches("Permission denied").count() >= 7,
        "outside file, another process, own auxv, a grandchild and a dial are all refused: {:?}",
        answers
            .split("\"role\":\"tool\"")
            .map(|t| t
                .rsplit("\"content\":")
                .next()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>())
            .collect::<Vec<_>>()
    );
    let fetched = seen
        .bodies
        .iter()
        .find(|b| b.contains("\"tool_call_id\":\"call-1-0\""))
        .expect("the web tool's result reached the model")
        .split("\"tool_call_id\":\"call-1-0\"")
        .next()
        .and_then(|before| before.rsplit("\"content\":").next())
        .unwrap_or("");
    assert!(
        fetched.contains("network.allow_hosts"),
        "the web tool is refused by the egress policy: {fetched}"
    );
}
