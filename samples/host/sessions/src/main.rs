// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sample-host-sessions`: a deploy desk serving two sessions at a time. See
//! the library documentation and README.md.
//!
//! ```text
//! sample-host-sessions --demo [--workspace DIR]         scripted model, no API key
//! sample-host-sessions --config FILE --workspace DIR    the model FILE configures
//! ```

use std::path::PathBuf;

use sample_host_sessions::{demo_config, run, Options};
use sven_sdk::Decision;

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
        None if args.demo => std::env::temp_dir().join("sample-host-sessions"),
        None => anyhow::bail!("pass --workspace DIR to keep the sessions in"),
    };
    // A rerun starts from nothing: stale markers would pass for fresh deploys.
    if workspace.exists() {
        std::fs::remove_dir_all(&workspace)?;
    }
    std::fs::create_dir_all(&workspace)?;

    let report = run(&Options {
        workspace: workspace.clone(),
        config,
    })
    .await?;

    println!("live: one engine, a gate handler per session");
    for gate in &report.routed {
        println!(
            "  {:<10} gate for {:<10} run {}  {}",
            gate.session,
            gate.service,
            gate.run_id,
            if gate.approved { "approved" } else { "refused" }
        );
    }
    println!("parked: suspended, restarted, decided from the stored state");
    for waiting in &report.decided {
        let what = match &waiting.decision {
            Decision::Approve => "approved".to_string(),
            Decision::Deny { reason } => format!("denied ({reason})"),
        };
        println!(
            "  {:<10} {:<10} {what}; the session ended {:?}",
            waiting.session, waiting.service, waiting.concluded
        );
    }
    println!(
        "event numbering carried on across the restart: {}",
        report.numbering_continued
    );
    println!("deployed: {}", report.deployed.join(", "));
    match &report.verified {
        Ok(()) => println!("check:    the desk did what its policy says"),
        Err(why) => println!("check:    FAILED - {why}"),
    }
    println!("workspace: {}", workspace.display());
    if report.verified.is_err() || !report.numbering_continued {
        std::process::exit(1);
    }
    Ok(())
}

struct Args {
    demo: bool,
    config: Option<PathBuf>,
    workspace: Option<PathBuf>,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
        let mut parsed = Args {
            demo: false,
            config: None,
            workspace: None,
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
                other => anyhow::bail!("unknown argument {other:?}; see README.md"),
            }
        }
        Ok(parsed)
    }
}
