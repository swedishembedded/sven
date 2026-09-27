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
mod events;
mod outcome;
mod provider;
mod runner;
mod store;
mod trace;

use runner::AttemptOptions;
use std::path::PathBuf;

const USAGE: &str = "\
usage: sample-agent-loop <command> [options]

  run --workspace DIR --task TEXT [options]
      delegate a task to the agent in DIR and write a structured outcome
  show [--run ID | --list]
      print a run's manifest, outcome and trace index
  resume --run ID [run options]
      continue an interrupted run from its checkpoint, reconciling against
      its own trace before acting

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
}

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
    let run_id = flags.run.clone().unwrap_or_default();
    let options = options_from(&flags, &run_id)?;
    let (outcome, _manifest) = runner::run(options)?;
    report(&outcome, flags.json)
}

fn resume(args: &[String]) -> anyhow::Result<()> {
    let flags = parse(args)?;
    let run_id = flags
        .run
        .clone()
        .ok_or_else(|| anyhow::anyhow!("resume needs --run ID"))?;
    let options = options_from(&flags, &run_id)?;
    let (outcome, _manifest) = runner::resume(&run_id, options)?;
    report(&outcome, flags.json)
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
        let adapter = flags.adapter.clone();
        Some(provider::LocalWeights {
            base,
            adapter,
            context_tokens: flags.context_tokens,
        })
    };
    // The api key follows the provider actually configured: OpenRouter's key
    // lives in its own environment name, everything else keeps the generic
    // one. Unset is not filled in here - a provider that needs a key fails
    // with its own error naming it.
    let api_key = flags
        .api_key
        .clone()
        .or_else(|| match flags.model.as_deref() {
            Some(spec) if spec.starts_with("openrouter/") => {
                std::env::var("AGENT_OPENROUTER_KEY").ok()
            }
            _ => std::env::var("BRAIN_API_KEY").ok(),
        });
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
}
