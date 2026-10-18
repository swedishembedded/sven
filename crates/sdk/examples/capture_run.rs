// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent trajectory capture for its clients. If
// your team needs expertise in turning agent runs into training data then you
// can procure our services by sending an email to info@swedishembedded.com.
//! Runs prompts against an OpenAI-compatible endpoint with sampling and
//! sampled-token capture on, and writes the resulting ATIF trajectory.
//!
//! This is the client half of `scripts/e2e/capture.sh`: it takes everything as
//! flags and keeps nothing but the trajectory.
//!
//! ```text
//! capture_run --endpoint URL --model NAME --api-key-file FILE --seed N \
//!             --temperature T --top-p P --top-k K --out FILE PROMPT...
//! ```

use std::path::PathBuf;

use sven_model::SamplingParams;
use sven_sdk::{Engine, RunOptions};

struct Args {
    endpoint: String,
    model: String,
    api_key: String,
    sampling: SamplingParams,
    out: PathBuf,
    prompts: Vec<String>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut it = std::env::args().skip(1);
    let (mut endpoint, mut model, mut key_file, mut out) = (None, None, None, None);
    let mut sampling = SamplingParams::default();
    let mut prompts = Vec::new();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .ok_or_else(|| anyhow::anyhow!("{name} needs a value"))
        };
        match arg.as_str() {
            "--endpoint" => endpoint = Some(value("--endpoint")?),
            "--model" => model = Some(value("--model")?),
            "--api-key-file" => key_file = Some(value("--api-key-file")?),
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--seed" => sampling.seed = Some(value("--seed")?.parse()?),
            "--temperature" => sampling.temperature = Some(value("--temperature")?.parse()?),
            "--top-p" => sampling.top_p = Some(value("--top-p")?.parse()?),
            "--top-k" => sampling.top_k = Some(value("--top-k")?.parse()?),
            flag if flag.starts_with("--") => anyhow::bail!("unknown flag {flag}"),
            prompt => prompts.push(prompt.to_string()),
        }
    }
    let key_file = key_file.ok_or_else(|| anyhow::anyhow!("--api-key-file is required"))?;
    anyhow::ensure!(!prompts.is_empty(), "give at least one prompt");
    Ok(Args {
        endpoint: endpoint.ok_or_else(|| anyhow::anyhow!("--endpoint is required"))?,
        model: model.ok_or_else(|| anyhow::anyhow!("--model is required"))?,
        api_key: std::fs::read_to_string(&key_file)?.trim().to_string(),
        sampling,
        out: out.ok_or_else(|| anyhow::anyhow!("--out is required"))?,
        prompts,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let mut config = sven_sdk::config::Config::default();
    config.model.provider = "openai".into();
    config.model.name = args.model;
    config.model.base_url = Some(args.endpoint);
    config.model.api_key = Some(args.api_key);
    let engine = Engine::builder().config(config).build()?;
    let mut agent = engine.agent("agent");
    for prompt in &args.prompts {
        let options = RunOptions::new()
            .sampling(args.sampling)
            .want_logprobs(true);
        agent.send_with(prompt, options).await?;
    }
    let trajectory = agent.trajectory();
    sven_sdk::atif::validate_trajectory(&trajectory)
        .map_err(|errors| anyhow::anyhow!("the trajectory is not valid ATIF: {errors:?}"))?;
    std::fs::write(&args.out, serde_json::to_vec_pretty(&trajectory)?)?;
    Ok(())
}
