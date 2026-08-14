// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use std::io::{self, Read};

use anyhow::Context;
use sven_ci::{MapOptions, ReduceOptions, TeeOptions};

// ── Map / Tee / Reduce command handlers ──────────────────────────────────────

/// `sven map TEMPLATE` - run one agent per stdin line.
pub(crate) async fn run_map_command(
    template: &str,
    concurrency: usize,
    model: Option<&str>,
    output_format: &str,
    separator: Option<&str>,
) -> anyhow::Result<()> {
    let stdin_data = read_stdin_to_string()?;

    let opts = MapOptions {
        template: template.to_string(),
        concurrency,
        model: model.map(|m| m.to_string()),
        sven_bin: None,
        extra_args: Vec::new(),
        output_format: output_format.to_string(),
        section_separator: separator.map(|s| s.to_string()),
    };

    sven_ci::pipe::run_map(opts, stdin_data).await
}

/// `sven tee CMD...` - broadcast stdin to N parallel commands.
pub(crate) async fn run_tee_command(
    commands: &[String],
    shell: &str,
    separator: Option<&str>,
) -> anyhow::Result<()> {
    let stdin_data = read_stdin_to_string()?;

    let opts = TeeOptions {
        commands: commands.to_vec(),
        shell: Some(shell.to_string()),
        section_separator: separator.map(|s| s.to_string()),
    };

    sven_ci::pipe::run_tee(opts, stdin_data).await
}

/// `sven reduce PROMPT` - aggregate stdin into one synthesis agent.
pub(crate) async fn run_reduce_command(
    prompt: &str,
    model: Option<&str>,
    output_format: &str,
    preamble: Option<&str>,
) -> anyhow::Result<()> {
    let stdin_data = read_stdin_to_string()?;

    let opts = ReduceOptions {
        prompt: prompt.to_string(),
        model: model.map(|m| m.to_string()),
        sven_bin: None,
        output_format: output_format.to_string(),
        preamble: preamble.map(|p| p.to_string()),
    };

    sven_ci::pipe::run_reduce(opts, stdin_data).await
}

/// Read all of stdin into a string.
pub(crate) fn read_stdin_to_string() -> anyhow::Result<String> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    Ok(buf)
}
