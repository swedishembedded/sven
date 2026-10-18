// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `repo-repair --model-endpoint URL [--model PROVIDER/NAME] [--api-key-file FILE]`: runs the
//! `repo-repair` harness on a fresh copy of its fixture repository with the
//! real coding tools and a model server you already run, then prints the
//! report and whether the tests really pass afterwards. See README.md.
//!
//! Swedish Embedded AB implements verifiable automated code-repair workflows
//! for its clients. If your team needs expertise in agent harnesses for
//! fixing failing builds then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;

use anyhow::{bail, Context as _};
use sample_harnesses_sdk_runner::repair;
use sven_sdk::config::Config;

/// The harness sample directory this binary repairs with.
fn harness_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../repo-repair")
}

fn copy_tree(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// `--flag value` out of `args`.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

async fn run(args: &[String]) -> anyhow::Result<bool> {
    let Some(endpoint) = flag(args, "--model-endpoint") else {
        bail!("usage: repo-repair --model-endpoint URL [--model PROVIDER/NAME] [--api-key-file FILE] [--keep]");
    };
    let spec = flag(args, "--model").unwrap_or_else(|| "ollama/qwen3:8b".to_string());
    let (provider, name) = spec.split_once('/').unwrap_or(("ollama", spec.as_str()));
    let mut config = Config::default();
    config.model.provider = provider.to_string();
    config.model.name = name.to_string();
    config.model.base_url = Some(endpoint);
    if let Some(file) = flag(args, "--api-key-file") {
        let key = std::fs::read_to_string(&file).with_context(|| format!("reading {file}"))?;
        config.model.api_key = Some(key.trim().to_string());
    }
    let model = sven_sdk::drivers::from_config(&config.model)
        .map_err(|e| anyhow::anyhow!("cannot build the model: {e:#}"))?;

    let work = tempfile::tempdir()?;
    let repo = work.path().canonicalize()?.join("repo");
    copy_tree(&harness_dir().join("fixtures/repo"), &repo)?;
    let report = repair(&harness_dir(), &repo, Arc::from(model)).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);

    // The harness's own verdict is not taken on trust: run the tests again.
    let tests = Command::new("sh")
        .arg("tests/test_total.sh")
        .current_dir(&repo)
        .output()
        .context("running the repaired repository's tests")?;
    println!(
        "tests after the run: {}",
        if tests.status.success() {
            "pass"
        } else {
            "fail"
        }
    );
    if args.iter().any(|a| a == "--keep") {
        println!("repository kept at {}", repo.display());
        std::mem::forget(work);
    }
    Ok(tests.status.success())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("repo-repair: {error:#}");
            ExitCode::from(2)
        }
    }
}
