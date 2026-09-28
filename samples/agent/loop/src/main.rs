// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents with a stable,
// documented invocation usable by people and by supervising models. If your
// team needs expertise in agent interfaces or CLI design, you can procure
// our services by sending an email to info@swedishembedded.com.

//! The loop agent: run a delegated workspace task through sven's engine,
//! trace every observable action, and hand back a structured result.
//!
//! ```text
//! sample-agent-loop run --workspace DIR --task TEXT [options]
//! sample-agent-loop show [--run ID | --list]
//! sample-agent-loop resume --run ID [run options]
//! ```
//!
//! Every run writes its complete trace under `~/.sven/loop/runs/<run_id>/`;
//! nothing depends on the process that wrote it still being alive.

mod clock;
pub(crate) mod eval;
mod events;
mod facts;
mod learn;
mod outcome;
mod provider;
mod runner;
mod store;
mod trace;
mod train;

use runner::AttemptOptions;
use std::path::PathBuf;
pub(crate) mod ask;

const USAGE: &str = "\
usage: sample-agent-loop <command> [options]

  run --workspace DIR --task TEXT [options]
      delegate a task to the agent in DIR and write a structured outcome
  show [--run ID | --list]
      print a run's manifest, outcome and trace index
  resume --run ID [run options]
      continue an interrupted run from its checkpoint, reconciling against
      its own trace before acting
  learn --run ID
      append a verified run's experience to the training pool (refuses a
      failed or unverified run)
  train [--dataset FILE] [--local-weights DIR] [--steps N] [--rank N]
        [--alpha F]
      fine-tune a LoRA adapter on the pool and promote it only when the
      held-out loss improved; non-zero exit on rejection
  explore --file FILE --out OUT.jsonl [--chunk-lines N]
      extract a question/answer training dataset from a markdown fact
      sheet: one JSONL record per fact, in the schema `learn` writes
  ask --question TEXT
      one-shot question; prints only the parsed {\"answer\": ...} JSON
      object; exit 2 when the reply is not strictly parseable
  eval-facts --dataset FILE.jsonl --out REPORT.json [--adapter FILE]
             [--shuffle] [--limit N] [--base]
      ask the configured model every question in a facts dataset and
      score each reply against its reference answer; writes a JSON
      report (per-question verdicts) and prints a one-line summary.
      Serves the promoted adapter by default; --base forces the
      untouched base model (the adapter-vs-base contrast)
  facts --file FILE [--work-dir DIR] [--out DATASET.jsonl]
        [--holdout-one-in N] [--steps N] [--rank N] [--alpha F]
        [--chunk-lines N] [model options as for run]
      learn a markdown fact sheet end to end: explore every fact,
      split (1-in-N held out), fine-tune a LoRA behind the held-out
      gate, and score recall on the trained questions plus
      generalization on the held-out ones. Artifacts and the JSON
      report land in --work-dir (default: facts/ under the state
      root); exit non-zero when the gate rejected the adapter

options for run / resume:
  --task-file FILE       read the task from FILE instead of --task
  --check CMD            completion check the run executes itself after the
                         turn (repeatable; the reviewer runs its own too)
  --local-weights DIR    local model checkpoint directory to serve the agent
                         from, in-process (default: $BRAIN_QWEN_WEIGHTS, else
                         ~/.local/share/brain/models/Qwen/Qwen3-0.6B)
  --adapter FILE         LoRA adapter folded into the local model at load
  --ctx N                inline context budget for the local model (default
                         16384)
  --model provider/name  run a REMOTE model instead of the local one (e.g.
                         openrouter/z-ai/glm-5.3-flash); needs its api key
  --base-url URL         served OpenAI-compatible endpoint (remote models)
  --api-key KEY          key for that endpoint (default: AGENT_OPENROUTER_KEY
                         for openrouter, BRAIN_API_KEY otherwise)
  --timeout-secs N       per-attempt wall-clock limit (default 600)
  --max-tool-rounds N    per-attempt tool-round limit (default: sven config)
  --record-input         capture the exact model input at the wire
                         (needs --base-url naming a proxied upstream)
  --json                 print the outcome as JSON on success

State lives under ~/.sven/loop/ (override: SVEN_LOOP_STATE). Every run has a
stable id; its manifest, trace, transcript, checkpoint and outcome survive
the process that wrote them.
";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("show") => show(&args[1..]),
        Some("resume") => resume(&args[1..]),
        Some("learn") => learn_cmd(&args[1..]),
        Some("train") => train_cmd(&args[1..]),
        Some("explore") => explore_cmd(&args[1..]),
        Some("ask") => ask_cmd(&args[1..]),
        Some("eval-facts") => eval_facts_cmd(&args[1..]),
        Some("facts") => facts_cmd(&args[1..]),
        Some("--help") | Some("-h") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown command {other:?}\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    }
}

struct Flags {
    workspace: Option<PathBuf>,
    task: Option<String>,
    checks: Vec<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    local_weights: Option<PathBuf>,
    adapter: Option<PathBuf>,
    context_tokens: u32,
    timeout_secs: u64,
    max_tool_rounds: Option<u32>,
    record_input: bool,
    json: bool,
    run: Option<String>,
    dataset: Option<PathBuf>,
    steps: u32,
    rank: u32,
    alpha: f32,
    file: Option<PathBuf>,
    out: Option<PathBuf>,
    chunk_lines: Option<usize>,
    question: Option<String>,
    shuffle: bool,
    limit: Option<usize>,
    work_dir: Option<PathBuf>,
    holdout_one_in: usize,
    force_base: bool,
}

pub(crate) mod explore;
const DEFAULT_TRAIN_STEPS: u32 = 40;
const DEFAULT_LORA_RANK: u32 = 8;
const DEFAULT_LORA_ALPHA: f32 = 16.0;
/// Every Nth explored fact is held out of training, so the pipeline's
/// generalization score always has something to measure.
const DEFAULT_HOLDOUT_ONE_IN: usize = 5;

fn parse(args: &[String]) -> anyhow::Result<Flags> {
    let mut flags = Flags {
        workspace: None,
        task: None,
        checks: Vec::new(),
        model: None,
        base_url: None,
        api_key: None,
        local_weights: None,
        adapter: None,
        context_tokens: 16_384,
        timeout_secs: 600,
        max_tool_rounds: None,
        record_input: false,
        json: false,
        run: None,
        dataset: None,
        steps: DEFAULT_TRAIN_STEPS,
        rank: DEFAULT_LORA_RANK,
        alpha: DEFAULT_LORA_ALPHA,
        file: None,
        out: None,
        chunk_lines: None,
        question: None,
        shuffle: false,
        limit: None,
        work_dir: None,
        holdout_one_in: DEFAULT_HOLDOUT_ONE_IN,
        force_base: false,
    };
    let mut i = 0;
    let mut task_file: Option<PathBuf> = None;
    while i < args.len() {
        let take = |i: &mut usize| -> anyhow::Result<String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{} needs a value", args[*i - 1]))
        };
        match args[i].as_str() {
            "--workspace" => flags.workspace = Some(PathBuf::from(take(&mut i)?)),
            "--task" => flags.task = Some(take(&mut i)?),
            "--task-file" => task_file = Some(PathBuf::from(take(&mut i)?)),
            "--check" => flags.checks.push(take(&mut i)?),
            "--model" => flags.model = Some(take(&mut i)?),
            "--base-url" => flags.base_url = Some(take(&mut i)?),
            "--api-key" => flags.api_key = Some(take(&mut i)?),
            "--local-weights" => flags.local_weights = Some(PathBuf::from(take(&mut i)?)),
            "--adapter" => flags.adapter = Some(PathBuf::from(take(&mut i)?)),
            "--ctx" => flags.context_tokens = take(&mut i)?.parse()?,
            "--timeout-secs" => flags.timeout_secs = take(&mut i)?.parse()?,
            "--max-tool-rounds" => flags.max_tool_rounds = Some(take(&mut i)?.parse()?),
            "--record-input" => flags.record_input = true,
            "--json" => flags.json = true,
            "--run" => flags.run = Some(take(&mut i)?),
            "--dataset" => flags.dataset = Some(PathBuf::from(take(&mut i)?)),
            "--steps" => flags.steps = take(&mut i)?.parse()?,
            "--rank" => flags.rank = take(&mut i)?.parse()?,
            "--alpha" => flags.alpha = take(&mut i)?.parse()?,
            "--file" => flags.file = Some(PathBuf::from(take(&mut i)?)),
            "--out" => flags.out = Some(PathBuf::from(take(&mut i)?)),
            "--chunk-lines" => flags.chunk_lines = Some(take(&mut i)?.parse()?),
            "--question" => flags.question = Some(take(&mut i)?),
            "--shuffle" => flags.shuffle = true,
            "--limit" => flags.limit = Some(take(&mut i)?.parse()?),
            "--work-dir" => flags.work_dir = Some(PathBuf::from(take(&mut i)?)),
            "--holdout-one-in" => flags.holdout_one_in = take(&mut i)?.parse()?,
            "--base" => flags.force_base = true,
            "--list" => {}
            other => anyhow::bail!("unknown option {other:?}"),
        }
        i += 1;
    }
    if let Some(path) = task_file {
        anyhow::ensure!(flags.task.is_none(), "--task and --task-file are exclusive");
        flags.task = Some(
            std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?,
        );
    }
    anyhow::ensure!(
        flags.adapter.is_none() || flags.model.is_none(),
        "--adapter applies to the local model; drop --model to run locally"
    );
    Ok(flags)
}

fn run(args: &[String]) -> anyhow::Result<()> {
    let flags = parse(args)?;
    allow_slow_local_prefill(&flags);
    let run_id = flags.run.clone().unwrap_or_default();
    let options = options_from(&flags, &run_id)?;
    let (outcome, _manifest) = runner::run(options)?;
    report(&outcome, flags.json)
}

fn resume(args: &[String]) -> anyhow::Result<()> {
    let flags = parse(args)?;
    allow_slow_local_prefill(&flags);
    let run_id = flags
        .run
        .clone()
        .ok_or_else(|| anyhow::anyhow!("resume needs --run ID"))?;
    let options = options_from(&flags, &run_id)?;
    let (outcome, _manifest) = runner::resume(&run_id, options)?;
    report(&outcome, flags.json)
}

/// `learn --run ID`: append a verified run's experience to the training
/// pool. Refuses anything the reviewer could not already trust.
fn learn_cmd(args: &[String]) -> anyhow::Result<()> {
    let flags = parse(args)?;
    let run_id = flags
        .run
        .clone()
        .ok_or_else(|| anyhow::anyhow!("learn needs --run ID"))?;
    match learn::learn_run(&run_id)? {
        learn::Learned::Appended => println!(
            "learned {run_id}: appended to {}",
            learn::pool_path().display()
        ),
        learn::Learned::AlreadyRecorded => {
            println!("learned {run_id}: already in the pool, no duplicate written")
        }
    }
    Ok(())
}

/// `train`: fine-tune a LoRA adapter on the pool and let the held-out gate
/// decide. Prints both scores either way; exits non-zero on rejection so a
/// delegating script never reads "worse model" as progress.
fn train_cmd(args: &[String]) -> anyhow::Result<()> {
    let flags = parse(args)?;
    let options = train::TrainOptions {
        model_dir: flags.local_weights.unwrap_or_else(default_local_weights),
        dataset: flags.dataset,
        steps: flags.steps,
        rank: flags.rank,
        alpha: flags.alpha,
    };
    let (decision, dir) = train::run(&options)?;
    match decision {
        train::Decision::Promoted => {
            println!("promoted: adapter and scores in {}", dir.display());
            Ok(())
        }
        train::Decision::Rejected => {
            println!(
                "rejected: held-out loss did not improve; scores in {}/decision.json",
                dir.display()
            );
            std::process::exit(1);
        }
    }
}

/// `explore --file FILE --out OUT.jsonl [--chunk-lines N]`: turn a markdown
/// fact sheet into question/answer training records, traced like a run.
fn explore_cmd(args: &[String]) -> anyhow::Result<()> {
    let flags = parse(args)?;
    let file = flags
        .file
        .clone()
        .ok_or_else(|| anyhow::anyhow!("explore needs --file FILE"))?;
    let out = flags
        .out
        .clone()
        .ok_or_else(|| anyhow::anyhow!("explore needs --out OUT.jsonl"))?;
    let options = explore::ExploreOptions {
        file,
        out,
        chunk_lines: flags.chunk_lines,
        model: flags.model.clone(),
        base_url: flags.base_url.clone(),
        api_key: api_key_of(&flags),
        local: local_weights_of(&flags),
    };
    let summary = explore::run(options)?;
    println!(
        "explored {}: {} section(s), {} fact(s), {} parse failure(s), \
         {} unanchored question(s) refused\nrun:     {}\nout:     {}",
        summary.run_id,
        summary.sections,
        summary.facts,
        summary.parse_failures,
        summary.unanchored,
        store::run_dir(&summary.run_id).display(),
        flags
            .out
            .map(|o| o.display().to_string())
            .unwrap_or_default(),
    );
    Ok(())
}

/// `ask --question TEXT`: one-shot strict-JSON question. Prints ONLY the
/// parsed object; exit 2 on an unparseable reply.
fn ask_cmd(args: &[String]) -> anyhow::Result<()> {
    let flags = parse(args)?;
    let question = flags
        .question
        .clone()
        .ok_or_else(|| anyhow::anyhow!("ask needs --question TEXT"))?;
    let options = ask::AskOptions {
        question,
        model: flags.model.clone(),
        base_url: flags.base_url.clone(),
        api_key: api_key_of(&flags),
        local: query_weights_of(&flags)?,
    };
    match ask::run(options) {
        Ok(answer) => {
            // Print the parsed object, not the raw reply - the caller reads
            // JSON or nothing.
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({ "answer": answer }))?
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("ask: {e:#}");
            std::process::exit(2);
        }
    }
}

/// `eval-facts --dataset FILE.jsonl --out REPORT.json [--adapter FILE]
/// [--shuffle] [--limit N]`: ask the model every dataset question and
/// score each reply against its reference, writing a JSON report with
/// per-question verdicts and printing a one-line summary.
fn eval_facts_cmd(args: &[String]) -> anyhow::Result<()> {
    let flags = parse(args)?;
    let dataset = flags
        .dataset
        .clone()
        .ok_or_else(|| anyhow::anyhow!("eval-facts needs --dataset FILE.jsonl"))?;
    let out = flags
        .out
        .clone()
        .ok_or_else(|| anyhow::anyhow!("eval-facts needs --out REPORT.json"))?;
    let options = eval::EvalOptions {
        dataset,
        out: out.clone(),
        model: flags.model.clone(),
        base_url: flags.base_url.clone(),
        api_key: api_key_of(&flags),
        local: query_weights_of(&flags)?,
        shuffle: flags.shuffle,
        limit: flags.limit,
    };
    let report = eval::run(options)?;
    println!(
        "eval-facts: {}/{} correct (accuracy {:.3}, {} parse failure(s)) - report in {}",
        report.correct,
        report.total,
        report.accuracy,
        report.parse_failures,
        out.display(),
    );
    Ok(())
}

/// `facts --file FILE`: the whole document-learning pipeline in one
/// command. Prints a two-line verdict - what was learned and what the
/// scores are - and exits non-zero when the gate rejected the adapter, so
/// a delegating script never reads a rejected candidate as progress.
fn facts_cmd(args: &[String]) -> anyhow::Result<()> {
    let flags = parse(args)?;
    let work_dir = flags
        .work_dir
        .clone()
        .unwrap_or_else(|| store::state_root().join("facts"));
    let options = facts::FactsOptions {
        file: flags.file.clone(),
        out: flags.out.clone(),
        work_dir,
        holdout_one_in: flags.holdout_one_in,
        chunk_lines: flags.chunk_lines,
        steps: flags.steps,
        rank: flags.rank,
        alpha: flags.alpha,
        model: flags.model.clone(),
        base_url: flags.base_url.clone(),
        api_key: api_key_of(&flags),
        local: local_weights_of(&flags),
    };
    let report = facts::run(options)?;
    println!(
        "facts: {} fact(s) from {} ({} train / {} eval), training {}",
        report.facts,
        flags
            .file
            .as_ref()
            .map(|f| f.display().to_string())
            .unwrap_or_else(|| "existing dataset".into()),
        report.train_records,
        report.eval_records,
        if report.promoted {
            format!("promoted ({})", report.train_id)
        } else {
            "REJECTED by the held-out gate".into()
        },
    );
    println!(
        "facts: recall {}/{} = {:.3}, holdout {}/{} = {:.3}",
        report.recall_correct,
        report.recall_total,
        report.recall_correct as f64 / report.recall_total.max(1) as f64,
        report.holdout_correct,
        report.holdout_total,
        report.holdout_correct as f64 / report.holdout_total.max(1) as f64,
    );
    if !report.promoted {
        std::process::exit(1);
    }
    Ok(())
}

/// The local weights `run` would serve from, when no `--model` was given -
/// the same local-first selection, including the `--adapter` promotion
/// pointer fold.
fn local_weights_of(flags: &Flags) -> Option<provider::LocalWeights> {
    if flags.model.is_some() {
        return None;
    }
    let base = flags
        .local_weights
        .clone()
        .or_else(|| {
            std::env::var("BRAIN_QWEN_WEIGHTS")
                .ok()
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(default_local_weights);
    Some(provider::LocalWeights {
        base,
        adapter: flags.adapter.clone(),
        context_tokens: flags.context_tokens,
    })
}

/// The local weights a QUESTION command (`ask`, `eval-facts`) serves from:
/// the promoted adapter by default, so querying what the pipeline learned
/// needs no flag at all. `--base` forces the untouched base model - the
/// adapter-vs-base contrast - and refuses to combine with an explicit
/// `--adapter`, which would leave the intent ambiguous. Delegating
/// commands (`run`, `explore`) keep `local_weights_of`: a facts adapter's
/// `{"answer": ...}` reply shape leaks into a coding loop and ends the
/// run answer-less, so serving it there by default would trade a working
/// agent for convenience.
fn query_weights_of(flags: &Flags) -> anyhow::Result<Option<provider::LocalWeights>> {
    anyhow::ensure!(
        !(flags.force_base && flags.adapter.is_some()),
        "--base and --adapter are exclusive: one names the base model, the other an adapter"
    );
    anyhow::ensure!(
        !(flags.force_base && flags.model.is_some()),
        "--base applies to the local model; drop --model to query locally"
    );
    let mut weights = local_weights_of(flags);
    if let Some(local) = weights.as_mut() {
        if local.adapter.is_none() && !flags.force_base {
            let pointer = train::adapter_pointer();
            if pointer.is_file() {
                local.adapter = Some(pointer);
            }
        }
    }
    Ok(weights)
}

/// The engine's stream watchdog declares a connection dead after 300 s of
/// silence between chunks - a guard for a REMOTE wire going stale. A local
/// provider is silent for a different reason: its prefill is one GPU submit
/// per prompt token, tens of seconds before the first chunk leaves the
/// process, and no chunk in between is honest to invent. When serving
/// locally, the attempt's own `--timeout-secs` is the bound that matters -
/// the timeout race stops the generation through the cancel token - so the
/// stream watchdog is raised to match instead of racing the prefill it was
/// never meant to judge. Set before the engine turns run, which read the
/// value per turn.
fn allow_slow_local_prefill(flags: &Flags) {
    if flags.model.is_none() {
        std::env::set_var(
            "SVEN_STREAM_CHUNK_TIMEOUT_SECS",
            flags.timeout_secs.to_string(),
        );
    }
}

/// The task a resume continues: an explicit `--task` wins; otherwise the
/// run's own recorded task. A resume must not require the caller to retype
/// what the run already carries - and a divergent retyping would silently
/// change what the recovered attempt works on.
fn task_for_resume(flags: &Flags, run_id: &str) -> anyhow::Result<String> {
    if let Some(task) = &flags.task {
        return Ok(task.clone());
    }
    Ok(store::read_manifest(run_id)?.task)
}

/// The adapter a resume serves from: an explicit `--adapter` wins; otherwise
/// the run's recorded one. Same rule as the task fallback - the caller must
/// not have to restate what the run carries, and dropping it silently turns
/// a resumed attempt into a base-model run while the manifest still says an
/// adapter rode along.
fn adapter_for_resume(flags: &Flags, run_id: &str) -> anyhow::Result<Option<std::path::PathBuf>> {
    if flags.adapter.is_some() {
        return Ok(flags.adapter.clone());
    }
    Ok(store::read_manifest(run_id)?.local_adapter)
}

/// The api key follows the provider actually configured: OpenRouter's key
/// lives in its own environment name, everything else keeps the generic
/// one. Unset is not filled in here - a provider that needs a key fails
/// with its own error naming it. Every model-touching command shares this
/// rule; only `run` used to honor it, which made `ask --model openrouter/...`
/// need an explicit --api-key the same call through `run` never did.
fn api_key_of(flags: &Flags) -> Option<String> {
    flags
        .api_key
        .clone()
        .or_else(|| match flags.model.as_deref() {
            Some(spec) if spec.starts_with("openrouter/") => {
                std::env::var("AGENT_OPENROUTER_KEY").ok()
            }
            _ => std::env::var("BRAIN_API_KEY").ok(),
        })
}

fn options_from(flags: &Flags, run_id: &str) -> anyhow::Result<AttemptOptions> {
    let workspace = flags
        .workspace
        .clone()
        .ok_or_else(|| anyhow::anyhow!("run needs --workspace DIR"))?;
    let task = match &flags.task {
        Some(task) => task.clone(),
        // Only a resume may fall back to the run's own task: a fresh run
        // without a task has nothing to continue.
        None if !run_id.is_empty() => task_for_resume(flags, run_id)?,
        None => anyhow::bail!("run needs --task TEXT or --task-file FILE"),
    };
    // Local-first: with no --model the attempt serves the agent from local
    // weights, in-process. An explicit --local-weights wins over the
    // environment, which wins over the documented default path.
    let local = if flags.model.is_some() {
        None
    } else {
        let base = flags
            .local_weights
            .clone()
            .or_else(|| {
                std::env::var("BRAIN_QWEN_WEIGHTS")
                    .ok()
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(default_local_weights);
        // A resume keeps the adapter the run was recorded with unless the
        // caller overrides it; see `adapter_for_resume`.
        let adapter = if run_id.is_empty() {
            flags.adapter.clone()
        } else {
            adapter_for_resume(flags, run_id)?
        };
        Some(provider::LocalWeights {
            base,
            adapter,
            context_tokens: flags.context_tokens,
        })
    };
    let api_key = api_key_of(flags);
    Ok(AttemptOptions {
        workspace,
        task,
        checks: flags.checks.clone(),
        model: flags.model.clone(),
        base_url: flags.base_url.clone(),
        api_key,
        local,
        timeout_secs: flags.timeout_secs,
        max_tool_rounds: flags.max_tool_rounds,
        record_input: flags.record_input,
    })
}

/// Where the default local weights live, when neither the caller nor the
/// environment names them: the model-store location brain itself uses.
fn default_local_weights() -> PathBuf {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => {
            PathBuf::from(home).join(".local/share/brain/models/Qwen/Qwen3-0.6B")
        }
        _ => PathBuf::from(".local/share/brain/models/Qwen/Qwen3-0.6B"),
    }
}

fn report(outcome: &outcome::Outcome, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(outcome)?);
    } else {
        report_human(outcome);
    }
    // Non-zero exit on a non-completed attempt - in BOTH output modes. A
    // script that delegated work must not read a timeout as success, and a
    // JSON consumer checks the exit code, not the prose.
    if outcome.status != outcome::Status::Completed {
        std::process::exit(1);
    }
    Ok(())
}

fn report_human(outcome: &outcome::Outcome) {
    println!("run:     {}", outcome.run_id);
    println!("status:  {}", outcome.status.as_str());
    if !outcome.changed_files.is_empty() {
        println!(
            "changed: {} file(s) ({})",
            outcome.changed_files.len(),
            outcome.changed_files_basis
        );
        for file in outcome.changed_files.iter().take(10) {
            println!("  {} ({})", file.path, file.kind);
        }
    }
    for check in &outcome.checks {
        println!(
            "check:   {} -> {} ({})",
            check.command,
            check.exit,
            if check.passed { "pass" } else { "FAIL" }
        );
    }
    println!(
        "usage:   {} tool call(s), {} failed, {} in / {} out tok{}",
        outcome.usage.tool_calls,
        outcome.usage.failed_tool_calls,
        outcome.usage.input_tokens,
        outcome.usage.output_tokens,
        outcome
            .usage
            .cost_usd
            .map(|c| format!(", ${c:.4}"))
            .unwrap_or_else(|| ", cost unmeasured".into()),
    );
    if !outcome.tool_failures.is_empty() {
        println!("failures:");
        for failure in outcome.tool_failures.iter().take(10) {
            println!("  {failure}");
        }
    }
    if !outcome.unresolved.is_empty() {
        println!("unresolved:");
        for issue in &outcome.unresolved {
            println!("  {issue}");
        }
    }
}

fn show(args: &[String]) -> anyhow::Result<()> {
    let flags = parse(args)?;
    if let Some(run_id) = &flags.run {
        let manifest = store::read_manifest(run_id)?;
        println!(
            "run:      {}\nstatus:   {}\nattempts: {}\nmodel:    {}\nstarted:  {}",
            manifest.run_id,
            manifest.status,
            manifest.attempts,
            manifest.model,
            manifest.started_ts,
        );
        println!("workspace: {}", manifest.workspace);
        println!("task: {}", manifest.task);
        let dir = store::run_dir(run_id);
        match outcome::Outcome::load(&dir) {
            Ok(outcome) => {
                println!(
                    "outcome:  {} ({} check(s), {} changed file(s))",
                    outcome.status.as_str(),
                    outcome.checks.len(),
                    outcome.changed_files.len()
                );
            }
            Err(e) => println!("outcome:  not written yet ({e})"),
        }
        let events = trace::read_events(&dir)?;
        println!("trace:    {} event(s); last:", events.len());
        if let Some(last) = events.last() {
            println!(
                "  [{} {} {}]",
                last.get("ts").and_then(|v| v.as_str()).unwrap_or("?"),
                last.get("seq").and_then(|v| v.as_u64()).unwrap_or(0),
                last.get("type").and_then(|v| v.as_str()).unwrap_or("?")
            );
        }
        return Ok(());
    }
    let runs = store::list_runs()?;
    if runs.is_empty() {
        println!("no runs recorded under {}", store::state_root().display());
        return Ok(());
    }
    for manifest in runs {
        println!(
            "{}  {:<10}  {}  {}",
            manifest.run_id,
            manifest.status,
            manifest.model,
            manifest.task.chars().take(60).collect::<String>()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resume without `--task` continues the run's own recorded task - the
    /// caller must not have to retype what the run carries, and a divergent
    /// retyping would silently change what the recovered attempt works on.
    #[test]
    fn a_resume_without_a_task_continues_the_runs_recorded_task() {
        let _guard = store::ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("loop-resume-task-{}", std::process::id()));
        std::env::set_var("SVEN_LOOP_STATE", &root);
        let dir = store::run_dir("loop-test-resume-task");
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = store::RunManifest {
            task: "confirm the sum".into(),
            ..Default::default()
        };
        store::write_atomic(
            &dir.join("run.json"),
            &serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();

        let with_task = task_for_resume(
            &parse(&["--task".into(), "explicit".into()]).unwrap(),
            "loop-test-resume-task",
        )
        .unwrap();
        assert_eq!(with_task, "explicit", "an explicit task wins");

        let defaulted = task_for_resume(&parse(&[]).unwrap(), "loop-test-resume-task").unwrap();
        assert_eq!(defaulted, "confirm the sum", "the run's own task continues");

        let unknown = task_for_resume(&parse(&[]).unwrap(), "loop-test-missing");
        assert!(
            unknown.is_err(),
            "a run with no manifest has no task to continue"
        );

        std::env::remove_var("SVEN_LOOP_STATE");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A resume without `--adapter` serves the adapter the run was recorded
    /// with - same rule as the task fallback: the caller must not have to
    /// restate what the run carries, and dropping it silently turns a
    /// resumed attempt into a base-model run while the manifest still says
    /// an adapter rode along.
    #[test]
    fn a_resume_without_an_adapter_serves_the_runs_recorded_adapter() {
        let _guard = store::ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("loop-resume-adapter-{}", std::process::id()));
        std::env::set_var("SVEN_LOOP_STATE", &root);
        let dir = store::run_dir("loop-test-resume-adapter");
        std::fs::create_dir_all(&dir).unwrap();
        let scratch =
            std::env::temp_dir().join(format!("loop-resume-adapter-{}", std::process::id()));
        let recorded = scratch.join("recorded-adapter.safetensors");
        let manifest = store::RunManifest {
            task: "fix it".into(),
            local_adapter: Some(recorded.clone()),
            ..Default::default()
        };
        store::write_atomic(
            &dir.join("run.json"),
            &serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();

        // Explicit flag wins over the recorded one.
        let explicit_path = scratch.join("explicit.safetensors");
        let explicit = parse(&["--adapter".into(), explicit_path.display().to_string()]).unwrap();
        assert_eq!(
            adapter_for_resume(&explicit, "loop-test-resume-adapter").unwrap(),
            Some(explicit_path),
            "an explicit adapter wins"
        );

        // No flag: the run's recorded adapter continues.
        let defaulted = parse(&[]).unwrap();
        assert_eq!(
            adapter_for_resume(&defaulted, "loop-test-resume-adapter").unwrap(),
            Some(recorded),
            "the run's recorded adapter continues"
        );

        // A run recorded without an adapter resumes on base weights, and an
        // unknown run id has nothing to fall back to.
        let bare = store::RunManifest {
            task: "fix it".into(),
            ..Default::default()
        };
        store::write_atomic(
            &dir.join("run.json"),
            &serde_json::to_string(&bare).unwrap(),
        )
        .unwrap();
        assert_eq!(
            adapter_for_resume(&defaulted, "loop-test-resume-adapter").unwrap(),
            None,
            "no recorded adapter resumes on base weights"
        );
        assert!(
            adapter_for_resume(&defaulted, "loop-test-missing").is_err(),
            "a run with no manifest has no adapter to continue"
        );

        std::env::remove_var("SVEN_LOOP_STATE");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A question command serves the promoted adapter by default - querying
    /// what the pipeline learned needs no flag - while `--base` forces the
    /// untouched base model for the contrast, and the two never combine
    /// with an explicit `--adapter`.
    #[test]
    fn question_commands_serve_the_promoted_adapter_until_base_is_asked() {
        let _guard = store::ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("loop-query-w-{}", std::process::id()));
        std::env::set_var("SVEN_LOOP_STATE", &root);
        let pointer = store::state_root().join("adapter.json");
        std::fs::create_dir_all(pointer.parent().unwrap()).unwrap();
        std::fs::write(
            &pointer,
            serde_json::to_string_pretty(&serde_json::json!({
                "adapter": "/somewhere/adapter.safetensors"
            }))
            .unwrap(),
        )
        .unwrap();

        // No flags: the promotion pointer rides along.
        let defaulted = parse(&[]).unwrap();
        let weights = query_weights_of(&defaulted).unwrap().unwrap();
        assert_eq!(
            weights.adapter,
            Some(pointer.clone()),
            "the promoted adapter is the default"
        );

        // --base: no adapter, even with a pointer in place.
        let base = parse(&["--base".into()]).unwrap();
        let weights = query_weights_of(&base).unwrap().unwrap();
        assert_eq!(weights.adapter, None, "--base serves the base model");

        // --adapter is explicit and wins over the default.
        let explicit = parse(&["--adapter".into(), "/mine.safetensors".into()]).unwrap();
        let weights = query_weights_of(&explicit).unwrap().unwrap();
        assert_eq!(
            weights.adapter,
            Some(std::path::PathBuf::from("/mine.safetensors"))
        );

        // The ambiguous combinations are refused, not resolved silently.
        assert!(query_weights_of(
            &parse(&["--base".into(), "--adapter".into(), "/m.safetensors".into()]).unwrap()
        )
        .is_err());
        assert!(query_weights_of(
            &parse(&[
                "--base".into(),
                "--model".into(),
                "openrouter/z-ai/glm-5.3-flash".into()
            ])
            .unwrap()
        )
        .is_err());

        // A delegating command keeps the base-only default even when a
        // pointer exists: a facts adapter's reply shape ends a coding run
        // answer-less.
        let run_flags = parse(&[]).unwrap();
        assert_eq!(
            local_weights_of(&run_flags).unwrap().adapter,
            None,
            "run/explore never serve the promotion pointer by default"
        );

        std::env::remove_var("SVEN_LOOP_STATE");
        let _ = std::fs::remove_dir_all(&root);
    }
}
