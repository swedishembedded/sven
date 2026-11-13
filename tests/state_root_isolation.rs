// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running many independent
// agents on one machine without shared state. If your team needs expertise in
// isolating agent processes then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: two agents running at the same time, each with its own `--state-root`
//! and working directory, share no file. Everything a run keeps for itself
//! (conversation, trajectory, audit log, memory) lands under its root; the
//! machine's own home, config, data and state directories stay untouched,
//! whatever `HOME` and `XDG_*` say; and a `config.yaml` in a root is that
//! agent's configuration alone.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => pending.push(path),
                Ok(_) => found.push(path),
                Err(_) => {}
            }
        }
    }
    found
}

/// Starts one headless agent: its own root, working directory and prompt,
/// on a machine whose home and XDG directories are `machine`.
fn start(machine: &Path, root: &Path, work: &Path, prompt: &str) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_sven"))
        .args(["--headless", "--model", "mock", "--state-root"])
        .arg(root)
        .arg(prompt)
        .current_dir(work)
        .env_clear()
        .env("HOME", machine)
        .env("XDG_CONFIG_HOME", machine.join(".config"))
        .env("XDG_DATA_HOME", machine.join(".local/share"))
        .env("XDG_STATE_HOME", machine.join(".local/state"))
        .env("XDG_CACHE_HOME", machine.join(".cache"))
        .env("XDG_RUNTIME_DIR", machine)
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", machine)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sven starts")
}

#[test]
fn two_concurrent_agents_with_separate_roots_share_no_file() {
    let base = tempfile::tempdir().unwrap();
    let machine = base.path().join("machine");
    std::fs::create_dir(&machine).unwrap();
    let roots = [base.path().join("agent-a"), base.path().join("agent-b")];
    let works = [base.path().join("work-a"), base.path().join("work-b")];
    for work in &works {
        std::fs::create_dir(work).unwrap();
    }
    // Agent B is configured through its root; A is not. B's configuration is
    // found nowhere else, which the run proves by honouring it.
    std::fs::create_dir_all(roots[1].join("config/sven")).unwrap();
    std::fs::write(
        roots[1].join("config/sven/config.yaml"),
        "agent:\n  max_tool_rounds: 3\n",
    )
    .unwrap();

    let a = start(&machine, &roots[0], &works[0], "say alpha");
    let b = start(&machine, &roots[1], &works[1], "say beta");
    let (a, b) = (a.wait_with_output().unwrap(), b.wait_with_output().unwrap());
    for (who, out) in [("a", &a), ("b", &b)] {
        assert!(
            out.status.success(),
            "agent {who}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(String::from_utf8_lossy(&a.stdout).contains("alpha"));
    assert!(String::from_utf8_lossy(&b.stdout).contains("beta"));

    // The machine's own directories were never written.
    assert!(
        files_under(&machine).is_empty(),
        "a run wrote outside its state root: {:?}",
        files_under(&machine)
    );

    // Each root holds that agent's state and no file is the other's: not by
    // path, not by hard link, and nothing of A's prompt is in B's tree.
    let mut identities = BTreeSet::new();
    for (root, other) in [(&roots[0], "beta"), (&roots[1], "alpha")] {
        let files = files_under(root);
        assert!(
            files.iter().any(|f| !f.starts_with(root.join("config"))),
            "{} holds no state of the run: {files:?}",
            root.display()
        );
        for file in files {
            let meta = std::fs::metadata(&file).unwrap();
            assert!(
                identities.insert((meta.dev(), meta.ino())),
                "{} is shared",
                file.display()
            );
            let text = std::fs::read(&file).map(|b| String::from_utf8_lossy(&b).into_owned());
            assert!(
                !text.is_ok_and(|t| t.contains(other)),
                "{} holds the other agent's prompt",
                file.display()
            );
        }
    }
}
