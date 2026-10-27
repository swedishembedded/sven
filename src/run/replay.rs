// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes whose faults can be replayed
// and reproduced on demand for its clients. If your team needs expertise in
// debugging LLM agents across the model stack then you can procure our
// services by sending an email to info@swedishembedded.com.
//! `sven replay`: send a recorded model request again.
//!
//! A run started with `--record-requests FILE` leaves every request exactly
//! as it was sent. Replaying the one before a fault, as often as needed and
//! against the current build of the model server and drivers, reproduces the
//! fault without the agent, its tools or its workspace; no tool runs.

use std::path::Path;

use anyhow::{bail, Context};
use futures::StreamExt;
use sven_model::{CompletionRequest, ModelProvider};
use sven_model_drivers::request_log::{read_log, Record, Response};

/// Which seed each replayed request carries.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ReplaySeed {
    /// The request's own sampling settings, as recorded.
    AsRecorded,
    /// The seed the server reported for the recorded response.
    Reported,
    /// This seed.
    Fixed(u64),
}

/// A recorded request with the response recorded for it.
struct Entry {
    request: CompletionRequest,
    response: Option<Response>,
}

fn entries(file: &Path) -> anyhow::Result<Vec<Entry>> {
    let mut out: Vec<(u64, Entry)> = Vec::new();
    for record in read_log(file)? {
        match record {
            Record::Request { seq, request, .. } => out.push((
                seq,
                Entry {
                    request: *request,
                    response: None,
                },
            )),
            // The latest open request of that number: logs appended by
            // several processes repeat sequence numbers.
            Record::Response { seq, response } => {
                if let Some((_, e)) = out
                    .iter_mut()
                    .rev()
                    .find(|(s, e)| *s == seq && e.response.is_none())
                {
                    e.response = Some(response);
                }
            }
        }
    }
    Ok(out.into_iter().map(|(_, e)| e).collect())
}

fn tail(text: &str, chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = flat.chars().count();
    if n <= chars {
        flat
    } else {
        format!("…{}", flat.chars().skip(n - chars).collect::<String>())
    }
}

fn summary(r: &Response) -> String {
    let calls: Vec<String> = r
        .tool_calls
        .iter()
        .map(|c| format!("{}({})", c.name, tail(&c.arguments, 60)))
        .collect();
    let mut line = format!(
        "seed={} in={} cached={} out={} calls=[{}] text={:?}",
        r.seed.map_or("-".into(), |s| s.to_string()),
        r.input_tokens,
        r.cache_read_tokens,
        r.output_tokens,
        calls.join(", "),
        tail(&r.text, 100),
    );
    if r.max_tokens {
        line.push_str(" max_tokens");
    }
    if let Some(e) = &r.error {
        line.push_str(&format!(" error={e:?}"));
    }
    line
}

/// Print the requests of `file`, one line each, numbered for `--request`.
pub(crate) fn list(file: &Path) -> anyhow::Result<()> {
    for (i, e) in entries(file)?.iter().enumerate() {
        let recorded = e
            .response
            .as_ref()
            .map_or("(no response recorded)".into(), summary);
        println!(
            "{:>4}  messages={:<4} {recorded}",
            i + 1,
            e.request.messages.len()
        );
    }
    Ok(())
}

/// Send request `index` (1-based; the last one when `None`) of `file`
/// `repeat` times to `model`, printing each response beside the recorded one.
pub(crate) async fn replay(
    file: &Path,
    index: Option<usize>,
    repeat: u32,
    seed: ReplaySeed,
    json: bool,
    model: &dyn ModelProvider,
) -> anyhow::Result<()> {
    let all = entries(file)?;
    if all.is_empty() {
        bail!("{} holds no recorded request", file.display());
    }
    let index = index.unwrap_or(all.len());
    let Some(entry) = index.checked_sub(1).and_then(|i| all.get(i)) else {
        bail!(
            "{} holds requests 1 to {}, not {index}",
            file.display(),
            all.len()
        );
    };
    let mut request = entry.request.clone();
    match seed {
        ReplaySeed::AsRecorded => {}
        ReplaySeed::Fixed(s) => request.sampling.seed = Some(s),
        ReplaySeed::Reported => {
            let reported = entry.response.as_ref().and_then(|r| r.seed);
            let Some(s) = reported else {
                bail!("request {index} has no seed reported for its response; pass --seed");
            };
            request.sampling.seed = Some(s);
        }
    }
    if !json {
        let recorded = entry.response.as_ref().map_or("(none)".into(), summary);
        println!(
            "request {index}: {} messages, recorded: {recorded}",
            request.messages.len()
        );
    }
    for attempt in 1..=repeat {
        let mut response = Response::default();
        match model.complete(request.clone()).await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(event) => response.observe(&event),
                        Err(e) => {
                            response.error = Some(format!("{e:#}"));
                            break;
                        }
                    }
                }
            }
            Err(e) => response.error = Some(format!("{e:#}")),
        }
        if json {
            let line =
                serde_json::json!({"request": index, "attempt": attempt, "response": response});
            println!("{line}");
        } else {
            println!("attempt {attempt}: {}", summary(&response));
        }
    }
    Ok(())
}

/// `sven replay`: list the requests of `file`, or replay one.
pub(crate) async fn run_replay_command(
    file: &Path,
    list_only: bool,
    index: Option<usize>,
    repeat: u32,
    seed: ReplaySeed,
    json: bool,
    config: &sven_bootstrap::Config,
) -> anyhow::Result<()> {
    if list_only {
        return list(file);
    }
    let model = sven_model_drivers::from_config_probed(&config.model)
        .await
        .context("building the model to replay against")?;
    replay(file, index, repeat, seed, json, model.as_ref()).await
}
