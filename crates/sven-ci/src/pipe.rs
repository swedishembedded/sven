// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Unix-philosophy pipe primitives for agent orchestration.
//!
//! These three commands — `map`, `tee`, `reduce` — complete Sven's
//! pipe composition story:
//!
//! ```bash
//! # Map: run one agent per changed file in parallel
//! git diff --name-only HEAD~1 \
//!   | sven map 'review {} for security issues'
//!
//! # Tee: broadcast one input to N specialised agents
//! sven 'analyse the codebase' --output-format compact \
//!   | sven tee \
//!       "sven 'find security issues'" \
//!       "sven 'find performance issues'"
//!
//! # Reduce: aggregate many agent outputs into one synthesis
//! sven map 'audit {}' | sven reduce 'prioritise these findings'
//! ```

use std::io::Write as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::Semaphore;

// ── MapOptions ────────────────────────────────────────────────────────────────

/// Options for `sven map`.
#[derive(Debug)]
pub struct MapOptions {
    /// Template string; `{}` is replaced with each stdin line.
    pub template: String,
    /// Maximum number of parallel sven instances (default: 4).
    pub concurrency: usize,
    /// Optional model override forwarded to each child instance.
    pub model: Option<String>,
    /// Path to sven binary (defaults to current executable).
    pub sven_bin: Option<PathBuf>,
    /// Additional flags forwarded verbatim to each child.
    pub extra_args: Vec<String>,
    /// Output format for child instances.  Defaults to `compact` so each
    /// child produces a single text section that is easy to aggregate.
    pub output_format: String,
    /// Separator written between each child's output section.
    pub section_separator: Option<String>,
}

impl Default for MapOptions {
    fn default() -> Self {
        Self {
            template: "{}".to_string(),
            concurrency: 4,
            model: None,
            sven_bin: None,
            extra_args: Vec::new(),
            output_format: "compact".to_string(),
            section_separator: None,
        }
    }
}

// ── TeeOptions ────────────────────────────────────────────────────────────────

/// Options for `sven tee`.
#[derive(Debug)]
pub struct TeeOptions {
    /// The shell commands to execute in parallel, each receiving the same stdin.
    /// Each entry is a full shell command string (e.g. `"sven 'find security issues'"`).
    pub commands: Vec<String>,
    /// Path to the shell binary used to execute each command (defaults to `sh`).
    pub shell: Option<String>,
    /// Separator written between each command's output section.
    pub section_separator: Option<String>,
}

// ── ReduceOptions ─────────────────────────────────────────────────────────────

/// Options for `sven reduce`.
#[derive(Debug)]
pub struct ReduceOptions {
    /// Synthesis prompt sent to the model along with all collected input.
    pub prompt: String,
    /// Optional model override.
    pub model: Option<String>,
    /// Path to sven binary (defaults to current executable).
    pub sven_bin: Option<PathBuf>,
    /// Output format for the final synthesis agent.
    pub output_format: String,
    /// Header prepended before the collected sections when passed to the agent.
    pub preamble: Option<String>,
}

impl Default for ReduceOptions {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            model: None,
            sven_bin: None,
            output_format: "compact".to_string(),
            preamble: None,
        }
    }
}

// ── sven map ──────────────────────────────────────────────────────────────────

/// Run `sven map`: spawn one sven agent per line from stdin.
///
/// Lines are distributed across at most `opts.concurrency` parallel instances.
/// Results are collected in order and written to stdout with optional separators.
pub async fn run_map(opts: MapOptions, stdin_data: String) -> anyhow::Result<()> {
    let sven_bin = resolve_sven_bin(opts.sven_bin.as_deref());

    let lines: Vec<String> = stdin_data
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    if lines.is_empty() {
        return Ok(());
    }

    let semaphore = Arc::new(Semaphore::new(opts.concurrency));
    let mut handles = Vec::with_capacity(lines.len());

    for line in lines {
        let permit = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .context("acquiring concurrency permit")?;
        let prompt = opts.template.replace("{}", &line);
        let sven = sven_bin.clone();
        let model = opts.model.clone();
        let fmt = opts.output_format.clone();
        let extra = opts.extra_args.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit; // hold until done
            run_child_agent(&sven, &prompt, model.as_deref(), &fmt, &extra, None).await
        });
        handles.push((line, handle));
    }

    let sep = opts
        .section_separator
        .unwrap_or_else(|| "\n---\n".to_string());

    let mut first = true;
    for (item, handle) in handles {
        let output = handle
            .await
            .context("child agent panicked")?
            .with_context(|| format!("child agent for {:?} failed", item))?;

        if !first {
            print!("{sep}");
        }
        first = false;
        print!("{output}");
        if !output.ends_with('\n') {
            println!();
        }
    }

    std::io::stdout().flush().ok();
    Ok(())
}

// ── sven tee ──────────────────────────────────────────────────────────────────

/// Run `sven tee`: broadcast the same stdin to N parallel shell commands.
///
/// Each command is run in a shell subprocess and receives an identical copy of
/// stdin.  Outputs are collected in order and written to stdout.
pub async fn run_tee(opts: TeeOptions, stdin_data: String) -> anyhow::Result<()> {
    if opts.commands.is_empty() {
        // With no commands, tee acts like cat — pass stdin through.
        print!("{stdin_data}");
        return Ok(());
    }

    let shell = opts.shell.as_deref().unwrap_or("sh");

    let mut handles = Vec::with_capacity(opts.commands.len());

    for cmd_str in &opts.commands {
        let shell = shell.to_string();
        let cmd_str = cmd_str.clone();
        let data = stdin_data.clone();

        let handle =
            tokio::spawn(
                async move { run_shell_command_with_stdin(&shell, &cmd_str, &data).await },
            );
        handles.push(handle);
    }

    let sep = opts
        .section_separator
        .unwrap_or_else(|| "\n---\n".to_string());

    let mut first = true;
    for handle in handles {
        let output = handle
            .await
            .context("tee command panicked")?
            .context("tee command failed")?;

        if !first {
            print!("{sep}");
        }
        first = false;
        print!("{output}");
        if !output.ends_with('\n') {
            println!();
        }
    }

    std::io::stdout().flush().ok();
    Ok(())
}

// ── sven reduce ───────────────────────────────────────────────────────────────

/// Run `sven reduce`: aggregate all stdin sections and pass to one synthesis agent.
///
/// The entire stdin is forwarded as context to a sven agent together with the
/// synthesis prompt.  The agent's response is the final output.
pub async fn run_reduce(opts: ReduceOptions, stdin_data: String) -> anyhow::Result<()> {
    let sven_bin = resolve_sven_bin(opts.sven_bin.as_deref());

    let context = if let Some(ref pre) = opts.preamble {
        format!("{pre}\n\n{stdin_data}")
    } else {
        stdin_data
    };

    // We pipe the context as stdin and provide the synthesis prompt as the CLI
    // argument.  The child agent auto-detects the piped format and seeds its
    // context accordingly.
    let output = run_child_agent(
        &sven_bin,
        &opts.prompt,
        opts.model.as_deref(),
        &opts.output_format,
        &[],
        Some(&context),
    )
    .await
    .context("reduce synthesis agent failed")?;

    print!("{output}");
    if !output.ends_with('\n') {
        println!();
    }
    std::io::stdout().flush().ok();
    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn resolve_sven_bin(override_path: Option<&std::path::Path>) -> String {
    if let Some(p) = override_path {
        return p.to_string_lossy().to_string();
    }
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "sven".to_string())
}

/// Spawn a child `sven` instance with the given prompt and collect its stdout.
///
/// If `stdin_data` is `Some`, it is written to the child's stdin so the child
/// can detect piped context (conversation markdown, JSONL, or plain text).
async fn run_child_agent(
    sven_bin: &str,
    prompt: &str,
    model: Option<&str>,
    output_format: &str,
    extra_args: &[String],
    stdin_data: Option<&str>,
) -> anyhow::Result<String> {
    let mut cmd = Command::new(sven_bin);
    cmd.arg("--headless")
        .arg("--output-format")
        .arg(output_format);

    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }

    for arg in extra_args {
        cmd.arg(arg);
    }

    // Prompt is the positional argument.
    cmd.arg(prompt);

    cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()); // forward progress/diagnostics to our stderr

    if stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn sven binary at {sven_bin:?}"))?;

    if let Some(data) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(data.as_bytes())
                .await
                .context("writing to child stdin")?;
        }
    }

    let mut stdout_buf = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout
            .read_to_string(&mut stdout_buf)
            .await
            .context("reading child stdout")?;
    }

    let status = child.wait().await.context("waiting for child")?;

    // Accept exit code 0 (success) and 3 (tool warnings) as non-fatal.
    // Exit code 3 means the run completed but had tool errors — we still
    // want the output.
    if !status.success() {
        let code = status.code().unwrap_or(-1);
        if code != 3 {
            anyhow::bail!(
                "sven child exited with code {code} for prompt {:?}",
                prompt.chars().take(80).collect::<String>()
            );
        }
    }

    Ok(stdout_buf)
}

/// Run a shell command string with the given stdin data, returning stdout.
async fn run_shell_command_with_stdin(
    shell: &str,
    cmd_str: &str,
    stdin_data: &str,
) -> anyhow::Result<String> {
    let mut child = Command::new(shell)
        .arg("-c")
        .arg(cmd_str)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to spawn shell command: {cmd_str:?}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_data.as_bytes())
            .await
            .context("writing to command stdin")?;
    }

    let mut stdout_buf = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout
            .read_to_string(&mut stdout_buf)
            .await
            .context("reading command stdout")?;
    }

    let status = child.wait().await.context("waiting for shell command")?;
    if !status.success() {
        let code = status.code().unwrap_or(-1);
        // Treat exit code 3 (tool warnings) as non-fatal.
        if code != 3 {
            anyhow::bail!(
                "shell command {:?} exited with code {code}",
                cmd_str.chars().take(80).collect::<String>()
            );
        }
    }

    Ok(stdout_buf)
}
