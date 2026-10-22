// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven init` and `sven config get|set`: the real binary in a throwaway home.
//!
//! The specs: with no configuration file a session says to run `sven init`;
//! `sven init` writes a `.sven.yaml` here naming an OpenRouter GLM model and
//! no key, and never overwrites one; `sven config set` writes exactly the one
//! key it is given into the file of its scope (project: here, workspace: the
//! nearest configuration above here, user: the user's own), refusing a key
//! the schema does not know and a value of the wrong type without touching
//! the file; `--stdin` reads the value from stdin, so a key never reaches the
//! shell history; `sven config get` prints the value in force, a default
//! included, and with `--scope` only what that file says.
//!
//! Swedish Embedded AB implements solutions for configuring coding agents
//! from the command line without hand-editing files, for its clients. If your
//! team needs expertise in agent configuration management then you can
//! procure our services by sending an email to info@swedishembedded.com.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A home with a workspace (`ws/`) holding a project directory (`ws/proj/`).
struct Machine(PathBuf);

impl Machine {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sven-config-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["home", "config", "run", "ws/proj"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        Self(dir)
    }

    fn project(&self) -> PathBuf {
        self.0.join("ws/proj")
    }

    fn sven(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sven"));
        cmd.current_dir(self.project())
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

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "sven {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn yaml(path: &Path) -> serde_yaml::Value {
    serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn with_no_configuration_file_a_session_says_to_run_sven_init() {
    let machine = Machine::new("hint");
    let out = machine.run(&["-H", "say hi"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("sven init"), "{stderr}");
}

#[test]
fn init_writes_an_openrouter_glm_model_with_no_key_and_never_overwrites() {
    let machine = Machine::new("init");
    let said = machine.ok(&["init"]);
    let file = machine.project().join(".sven.yaml");
    let written = yaml(&file);
    assert_eq!(written["model"]["provider"], "openrouter");
    assert!(
        written["model"]["name"].as_str().unwrap().contains("glm"),
        "{written:?}"
    );
    assert!(
        written["model"].get("api_key").is_none(),
        "no key is written: {written:?}"
    );
    assert!(
        said.contains("sven auth set openrouter"),
        "init says how to give the key: {said}"
    );

    std::fs::write(&file, "model:\n  name: mine\n").unwrap();
    let again = machine.run(&["init"]);
    assert_ne!(again.status.code(), Some(0), "a second init refuses");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "model:\n  name: mine\n",
        "and leaves the file as it was"
    );

    let out = machine.run(&["-H", "say hi"]);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("sven init"),
        "no hint once a file exists"
    );
}

#[test]
fn set_writes_only_the_key_it_is_given_and_get_reads_what_is_in_force() {
    let machine = Machine::new("set");
    machine.ok(&["config", "set", "model.temperature", "0.3"]);
    let file = machine.project().join(".sven.yaml");
    let written = yaml(&file);
    assert_eq!(
        written["model"]["temperature"],
        serde_yaml::Value::from(0.3)
    );
    assert_eq!(
        written.as_mapping().unwrap().len(),
        1,
        "only what was set: {written:?}"
    );
    assert_eq!(
        written["model"].as_mapping().unwrap().len(),
        1,
        "only what was set: {written:?}"
    );

    assert_eq!(machine.ok(&["config", "get", "model.temperature"]), "0.3");
    assert_eq!(
        machine.ok(&["config", "get", "agent.max_tool_rounds"]),
        "200",
        "a default is in force too"
    );

    machine.ok(&["config", "set", "model.name", "z-ai/glm-5.3"]);
    let written = yaml(&file);
    assert_eq!(written["model"]["name"], "z-ai/glm-5.3");
    assert_eq!(
        written["model"]["temperature"],
        serde_yaml::Value::from(0.3),
        "an earlier key is kept"
    );
}

#[test]
fn a_key_the_schema_does_not_know_or_a_value_of_the_wrong_type_is_refused_untouched() {
    let machine = Machine::new("refuse");
    machine.ok(&["config", "set", "model.temperature", "0.3"]);
    let file = machine.project().join(".sven.yaml");
    let before = std::fs::read_to_string(&file).unwrap();

    let out = machine.run(&["config", "set", "model.temprature", "0.5"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("model.temprature"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = machine.run(&["config", "set", "model.temperature", "hot"]);
    assert_ne!(out.status.code(), Some(0), "a string is no temperature");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        before,
        "a refused set leaves the file as it was"
    );
    assert_ne!(
        machine.run(&["config", "get", "model.nope"]).status.code(),
        Some(0)
    );
}

#[test]
fn each_scope_writes_its_own_file_and_the_nearer_one_wins() {
    let machine = Machine::new("scopes");
    std::fs::write(
        machine.0.join("ws/.sven.yaml"),
        "model:\n  name: from-workspace\n",
    )
    .unwrap();
    assert_eq!(
        machine.ok(&["config", "get", "model.name"]),
        "from-workspace",
        "the workspace layer is read"
    );

    machine.ok(&[
        "config",
        "set",
        "--scope",
        "workspace",
        "agent.max_tool_rounds",
        "7",
    ]);
    assert_eq!(
        yaml(&machine.0.join("ws/.sven.yaml"))["agent"]["max_tool_rounds"],
        7
    );
    assert!(
        !machine.project().join(".sven.yaml").exists(),
        "the project scope was not touched"
    );

    machine.ok(&["config", "set", "--scope", "user", "tui.wrap_width", "100"]);
    assert_eq!(
        yaml(&machine.0.join("config/sven/config.yaml"))["tui"]["wrap_width"],
        100
    );

    machine.ok(&["config", "set", "model.name", "from-project"]);
    assert_eq!(
        machine.ok(&["config", "get", "model.name"]),
        "from-project",
        "project over workspace"
    );
    assert_eq!(
        machine.ok(&["config", "get", "--scope", "workspace", "model.name"]),
        "from-workspace"
    );
    let unset = machine.run(&["config", "get", "--scope", "user", "model.name"]);
    assert_ne!(
        unset.status.code(),
        Some(0),
        "a scope that does not set it says so"
    );
}

#[test]
fn a_workspace_scope_with_no_configuration_above_is_refused() {
    let machine = Machine::new("no-workspace");
    let out = machine.run(&["config", "set", "--scope", "workspace", "model.name", "x"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("workspace"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn stdin_carries_a_secret_that_never_appears_on_the_command_line() {
    let machine = Machine::new("stdin");
    let mut child = machine
        .sven(&["config", "set", "model.api_key", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"sk-or-from-stdin\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("sk-or-from-stdin"),
        "never echoed"
    );
    assert_eq!(
        yaml(&machine.project().join(".sven.yaml"))["model"]["api_key"],
        "sk-or-from-stdin"
    );
}

#[test]
fn a_key_with_a_dot_in_it_is_quoted() {
    let machine = Machine::new("quoted");
    machine.ok(&["config", "set", "providers.or.name", "openrouter"]);
    machine.ok(&[
        "config",
        "set",
        r#"providers.or.models."z-ai/glm-5.3".max_tokens"#,
        "8192",
    ]);
    let written = yaml(&machine.project().join(".sven.yaml"));
    assert_eq!(
        written["providers"]["or"]["models"]["z-ai/glm-5.3"]["max_tokens"],
        8192
    );
    assert_eq!(
        machine.ok(&[
            "config",
            "get",
            r#"providers.or.models."z-ai/glm-5.3".max_tokens"#
        ]),
        "8192"
    );
}
