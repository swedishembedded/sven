// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven task run` - the CLI entry point for the verified-task machine.
//!
//! This is the one place a `.task.toml` file's bytes are ever read. Reading
//! (I/O) happens here, outside the kernel; the verified-task machine's
//! `Freeze` state only ever parses an already-provided
//! [`sven_vocab::verify::VerifiedTaskSeed`] JSON string, which is what keeps
//! that transition pure. Typing `sven task run` is the human act that mints
//! the seed's [`sven_vocab::provenance::ContentDigest`] - a content digest of
//! the exact file bytes read, never a value the model could influence.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sven_config::{AgentMode, Config};
use sven_vocab::provenance::ContentDigest;
use sven_vocab::verify::{Task, VerifiedTaskSeed, VerifierSpec};

use crate::cli::TaskCommands;

/// On-disk shape of a `.task.toml` file. Mirrors [`Task`] except
/// `max_attempts` is optional here too (TOML has no notion of a serde
/// container default the way JSON deserialization does through `Task`'s own
/// `#[serde(default)]`, so it is re-declared explicitly).
#[derive(Debug, Deserialize)]
struct TaskFile {
    id: String,
    prompt: String,
    verifier: VerifierSpec,
    #[serde(default = "Task::default_max_attempts")]
    max_attempts: u32,
}

pub(crate) async fn run_task_command(cmd: &TaskCommands, config: &Config) -> anyhow::Result<()> {
    match cmd {
        TaskCommands::Run { file, project_root, model, timeout } => {
            run(file, project_root.as_deref(), model.as_deref(), *timeout, config).await
        }
    }
}

async fn run(
    file: &Path,
    project_root: Option<&Path>,
    model: Option<&str>,
    timeout_secs: Option<u64>,
    config: &Config,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(file)
        .with_context(|| format!("reading task file {}", file.display()))?;
    let source_digest = ContentDigest::from_hex(hex::encode(Sha256::digest(&bytes)));

    let text = String::from_utf8(bytes)
        .with_context(|| format!("{} is not valid UTF-8", file.display()))?;
    let parsed: TaskFile = toml::from_str(&text)
        .with_context(|| format!("parsing {} as a task file", file.display()))?;

    let task = Task {
        id: parsed.id,
        prompt: parsed.prompt,
        verifier: parsed.verifier,
        max_attempts: parsed.max_attempts,
    };
    let seed = VerifiedTaskSeed { task, source_digest };
    let seed_json = serde_json::to_string(&seed).context("serializing the task seed")?;

    let project_root = project_root
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());

    let kernel_config = if let Some(m) = model {
        let mut cfg = config.clone();
        cfg.model = sven_model::resolve_model_from_config(&cfg, m);
        std::sync::Arc::new(cfg)
    } else {
        std::sync::Arc::new(config.clone())
    };

    let runner = sven_ci::RuntimeRunner::new(kernel_config);
    let code = runner
        .run(sven_ci::RuntimeRunnerOptions {
            mode: "verified-task".to_string(),
            agent_mode: AgentMode::Agent,
            prompt: seed_json,
            history: Vec::new(),
            project_root,
            timeout_secs,
            step_timeout_secs: None,
            max_tokens_budget: None,
            append_system_prompt: None,
            no_system: false,
            no_tools: false,
            trace_level: 0,
        })
        .await;
    std::process::exit(code);
}
