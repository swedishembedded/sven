// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sample-agent-seat`: see the library documentation and README.md.
//!
//! ```text
//! sample-agent-seat --learner-config FILE (--spec FILE --seat-config FILE | --live)
//!                   [--learner-role TEXT] [--probes FILE] [--rounds N]
//!                   [--consent] [--retention-days N]
//! ```
//! Prints the output as JSON on stdout; with `--live` the conversation is on
//! the terminal (stdin and stderr).

use std::path::PathBuf;

use anyhow::{bail, Context};
use sample_agent_seat::{run, Probe, SeatChoice};
use sven_sdk::seat::LiveTerms;

fn main_args() -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let Some(name) = flag.strip_prefix("--") else {
            bail!("unexpected argument {flag}");
        };
        let value = match name {
            "live" | "consent" => String::new(),
            _ => args
                .next()
                .with_context(|| format!("--{name} needs a value"))?,
        };
        map.insert(name.to_string(), value);
    }
    Ok(map)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = main_args()?;
    let load = |key: &str| -> anyhow::Result<_> {
        let path = args
            .get(key)
            .with_context(|| format!("pass --{key} FILE"))?;
        sven_sdk::config::load(Some(&PathBuf::from(path)))
    };
    let learner_config = load("learner-config")?;
    let spec_path = args.get("spec").map(PathBuf::from);
    let seat = if args.contains_key("live") {
        SeatChoice::Live(LiveTerms {
            consent_recorded: args.contains_key("consent"),
            retention_days: args.get("retention-days").map(|d| d.parse()).transpose()?,
        })
    } else {
        SeatChoice::Simulated {
            spec: spec_path
                .as_deref()
                .context("pass --spec FILE, or --live")?,
            config: Box::new(load("seat-config")?),
        }
    };
    let probes: Vec<Probe> = match args.get("probes") {
        Some(path) => serde_yaml::from_str(&std::fs::read_to_string(path)?)?,
        None => Vec::new(),
    };
    let rounds = args.get("rounds").map_or(Ok(8), |r| r.parse())?;
    let output = run(
        learner_config,
        args.get("learner-role").map(String::as_str),
        seat,
        &probes,
        rounds,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
