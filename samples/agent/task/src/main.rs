// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sample-agent-task`: an agent drafts and publishes release notes. See the
//! library documentation and README.md.
//!
//! ```text
//! sample-agent-task --demo [--workspace DIR]         scripted model, no API key
//! sample-agent-task --config FILE --workspace DIR    the model FILE configures
//!                   [--answer TEXT] [--deadline SECS]
//! ```

use std::path::PathBuf;
use std::time::Duration;

use sample_agent_task::{demo_config, run, Options, CHANGES, DEMO_CHANGES};
use sven_sdk::{CancelToken, RunOutcome};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse(std::env::args().skip(1))?;

    let config = match (&args.config, args.demo) {
        (Some(path), _) => sven_sdk::config::load(Some(path))?,
        (None, true) => demo_config(),
        (None, false) => anyhow::bail!("pass --demo, or --config FILE naming a model"),
    };
    let workspace = match args.workspace {
        Some(dir) => dir,
        None if args.demo => std::env::temp_dir().join("sample-agent-task"),
        None => anyhow::bail!("pass --workspace DIR containing {CHANGES}"),
    };
    std::fs::create_dir_all(&workspace)?;
    if args.demo && !workspace.join(CHANGES).exists() {
        std::fs::write(workspace.join(CHANGES), DEMO_CHANGES)?;
    }

    // Ctrl-C cancels the run in progress; the agent keeps what it had.
    let cancel = CancelToken::new();
    let on_signal = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_signal.cancel();
        }
    });

    let report = run(&Options {
        workspace: workspace.clone(),
        config,
        answer: args.answer,
        cancel,
        deadline: args.deadline,
    })
    .await?;

    show("draft", &report.drafted);
    if let Some(answered) = &report.answered {
        show("answered", answered);
    }
    show("publish", &report.published);
    match &report.verified {
        Ok(()) => println!("check:    the notes cover every change"),
        Err(why) => println!("check:    FAILED - {why}"),
    }
    println!("trace:    {}", report.trajectory.display());
    println!("workspace: {}", workspace.display());
    if report.verified.is_err() {
        std::process::exit(1);
    }
    Ok(())
}

fn show(step: &str, outcome: &RunOutcome) {
    let tokens = match (outcome.usage.input_tokens, outcome.usage.output_tokens) {
        (Some(i), Some(o)) => format!("{i} in / {o} out"),
        _ => "not reported".to_string(),
    };
    println!("{step:<9} {:?} ({tokens})", outcome.conclusion);
    if let Some(question) = &outcome.question {
        println!(
            "          asked: {} {:?}",
            question.prompt, question.options
        );
    }
    if !outcome.reply.is_empty() {
        println!("          {}", outcome.reply.trim());
    }
}

struct Args {
    demo: bool,
    config: Option<PathBuf>,
    workspace: Option<PathBuf>,
    answer: String,
    deadline: Duration,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
        let mut parsed = Args {
            demo: false,
            config: None,
            workspace: None,
            answer: "users".to_string(),
            deadline: Duration::from_secs(300),
        };
        while let Some(arg) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| anyhow::anyhow!("{arg} needs a value"))
            };
            match arg.as_str() {
                "--demo" => parsed.demo = true,
                "--config" => parsed.config = Some(PathBuf::from(value()?)),
                "--workspace" => parsed.workspace = Some(PathBuf::from(value()?)),
                "--answer" => parsed.answer = value()?,
                "--deadline" => parsed.deadline = Duration::from_secs(value()?.parse()?),
                other => anyhow::bail!("unknown argument {other:?}; see README.md"),
            }
        }
        Ok(parsed)
    }
}
