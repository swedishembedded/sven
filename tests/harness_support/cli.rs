// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Running `sven harness` the way a person does, for the CLI specs: the binary
//! in a scratch directory with no model environment and no user
//! configuration, the committed samples and fixtures, and readers of what it
//! printed.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/harness")
}

pub fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples/harnesses")
        .join(name)
}

/// The binary, in `cwd`, with no model environment and no user config.
pub fn sven(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sven"));
    cmd.current_dir(cwd)
        .env("HOME", cwd)
        .env("XDG_CONFIG_HOME", cwd)
        .env_remove("SVEN_MODEL")
        .arg("harness");
    cmd
}

pub fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("the sven binary runs")
}

pub fn code(out: &Output) -> i32 {
    out.status.code().expect("exited normally")
}

pub fn stdout_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is pure JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}
