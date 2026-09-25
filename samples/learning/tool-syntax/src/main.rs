// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! Does learning from its own verified experience teach a small model to find
//! out what it needs before acting?
//!
//! The task the model is given cannot be answered by reading. Three deployment
//! configurations sit in the workspace and nothing says which one the host is
//! running; the only way to find out is to ask the service. A model that
//! guesses has a one-in-two chance on any instance and cannot do better than
//! chance across a matched pair - which is what makes the score mean
//! something.
//!
//! Subcommands, in the order they are meant to be used:
//!
//! ```text
//! audit      the task catalog checks itself. No model, no GPU, no network.
//! baseline   what the base model scores, before anything is learned.
//! ```
//!
//! Swedish Embedded AB implements closed-loop learning systems - agents that
//! improve from their own verified experience rather than from hand-written
//! training data - for its clients. If your team needs expertise in agent
//! training loops, experiment design, or promotion gating for small models,
//! you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::path::PathBuf;

use sample_learning_lab::{
    baseline_effective, run_verifier, run_witness, ArmScore, Episode, Family, Outcome, ServedModel,
};
use sven_sdk::{config, ApprovalPolicy, Engine, SessionEvent};

const USAGE: &str = "\
usage: sample-learning-tool-syntax <command> [options]

  audit                    check the task catalog: no model, no GPU, no network
  baseline [options]       measure the base model on the frozen instances

options for baseline:
  --instances N            episodes per twin (default 4)
  --run-dir DIR            where episodes are materialised (default: a temp dir)
  --model ID               resident manifest id (default brain/qwen3)
  --base-url URL           the served endpoint (default: sven's own configuration)
  --api-key KEY            key for that endpoint (default: BRAIN_API_KEY)
  --report FILE.json       write the machine-readable result here
  --hint TEXT              appended to the request for THIS arm only

A hint is an exploration aid, not a change to the task. Collection may use one;
the measured arms must not, or the number says what the prompt did rather than
what the model learned.

The model is ordinary sven configuration, not a flag: point sven at a served
brain and the same binary measures whatever it is serving. The id must name the
RESIDENT (brain/qwen3), not the model-store spelling - only the resident ever
receives a trained adapter.
";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("audit") => audit(),
        Some("baseline") => baseline(&args[1..]),
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

fn family_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the learning workspace")
        .join("tasks/config-discovery")
}

/// The catalog checking itself. Delegates to the family's own audit, which is
/// the same one `make samples/learning/audit` runs - there is one audit, not
/// one per caller.
fn audit() -> anyhow::Result<()> {
    let root = family_root();
    let family = Family::load(&root).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "family: {} ({} predicates)",
        family.id(),
        family.predicates().names().len()
    );

    let status = std::process::Command::new("python3")
        .arg(root.join("world/audit.py"))
        .current_dir(&root)
        .status()?;
    if !status.success() {
        anyhow::bail!("the task catalog did not pass its own audit");
    }
    Ok(())
}

struct BaselineOptions {
    instances: usize,
    run_dir: PathBuf,
    model: ServedModel,
    /// Named rather than auto-detected on purpose. An experiment that lets
    /// its endpoint be discovered is an experiment that cannot say afterwards
    /// which server produced the number - and on a shared machine the answer
    /// changes between runs.
    base_url: Option<String>,
    api_key: Option<String>,
    report: Option<PathBuf>,
    /// Appended to the frozen request. Recorded in the report so an arm that
    /// used one can never be mistaken for one that did not.
    hint: Option<String>,
}

fn parse_baseline(args: &[String]) -> anyhow::Result<BaselineOptions> {
    let mut instances = 4usize;
    let mut run_dir = std::env::temp_dir().join(format!("tool-syntax-{}", std::process::id()));
    let mut model = ServedModel::qwen3();
    let mut base_url = None;
    let mut api_key = std::env::var("BRAIN_API_KEY").ok();
    let mut report = None;
    let mut hint = None;

    let mut i = 0;
    while i < args.len() {
        let take = |i: &mut usize| -> anyhow::Result<String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{} needs a value", args[*i - 1]))
        };
        match args[i].as_str() {
            "--instances" => instances = take(&mut i)?.parse()?,
            "--run-dir" => run_dir = PathBuf::from(take(&mut i)?),
            "--model" => model = ServedModel::resident(take(&mut i)?),
            "--base-url" => base_url = Some(take(&mut i)?),
            "--api-key" => api_key = Some(take(&mut i)?),
            "--report" => report = Some(PathBuf::from(take(&mut i)?)),
            "--hint" => hint = Some(take(&mut i)?),
            other => anyhow::bail!("unknown option {other:?}"),
        }
        i += 1;
    }
    Ok(BaselineOptions {
        instances,
        run_dir,
        model,
        base_url,
        api_key,
        report,
        hint,
    })
}

fn baseline(args: &[String]) -> anyhow::Result<()> {
    let options = parse_baseline(args)?;
    let family = Family::load(&family_root()).map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("baseline: {} on {}", family.id(), options.model);
    println!("request: {}", family.request().replace('\n', " "));
    println!();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // Control: the environment, solved by something that is not a model. If
    // the witness cannot solve an instance, everything below measures a broken
    // harness - and a score of zero looks exactly like a model that could not
    // do the task.
    {
        let dir = options.run_dir.join("control-witness");
        let episode = Episode::start(&family, &dir, &family.live_choices()[0])
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        match run_witness(&family, &episode) {
            Ok(calls) => {
                println!("control: the witness solves this environment in {calls} call(s)")
            }
            Err(e) => anyhow::bail!(
                "the witness cannot solve this environment ({e}); every number below would \
                 measure the harness rather than the model"
            ),
        }
        println!();
    }

    let mut score = ArmScore::new("baseline", options.model.api_model());
    let mut records = Vec::new();

    for (index, hidden) in family
        .live_choices()
        .iter()
        .cycle()
        .take(options.instances * family.live_choices().len())
        .enumerate()
    {
        let dir = options.run_dir.join(format!("ep-{index:03}-{hidden}"));
        let outcome = runtime.block_on(one_episode(&family, &dir, hidden, &options));
        let (outcome, detail) = match outcome {
            Ok((verdict, detail)) => (Outcome::from_verdict(&verdict), detail),
            Err(e) => (Outcome::errored(e.to_string()), format!("{e}")),
        };
        let mark = match &outcome {
            Outcome::Answered { solved: true } => "PASS",
            Outcome::Answered { solved: false } => "fail",
            Outcome::Errored { .. } => "ERROR",
        };
        println!("  ep-{index:03} [{hidden:<10}] {mark}  {detail}");
        score.record(&outcome);
        records.push(serde_json::json!({
            "episode": index, "hidden_state": hidden, "outcome": outcome, "detail": detail,
        }));
    }

    println!();
    match score.rate() {
        Some(rate) => println!(
            "baseline: {}/{} solved ({:.0}%) on {}",
            score.solved,
            score.denominator(),
            rate * 100.0,
            options.model
        ),
        None => println!("baseline: nothing reached a verdict; there is no score to report"),
    }
    if let Some(caveat) = score.caveat() {
        eprintln!("{caveat}");
    }

    if let Some(path) = options.report {
        let body = serde_json::json!({
            "family": family.id(), "arm": "baseline", "model": options.model.api_model(),
            "hint": options.hint,
            "score": {
                "solved": score.solved, "answered": score.answered, "errored": score.errored,
                "rate": score.rate(),
            },
            "episodes": records,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&body)? + "\n")?;
        println!("report: {}", path.display());
    }
    Ok(())
}

/// One episode, start to verdict.
async fn one_episode(
    family: &Family,
    dir: &std::path::Path,
    hidden: &str,
    options: &BaselineOptions,
) -> anyhow::Result<(sample_learning_lab::Verdict, String)> {
    let mut episode = Episode::start(family, dir, hidden).map_err(|e| anyhow::anyhow!("{e}"))?;
    let before = baseline_effective(family, &episode).map_err(|e| anyhow::anyhow!("{e}"))?;

    // The world is reachable only through the workspace's own CLI, and only
    // because these variables are set for the agent's tools.
    for (key, value) in episode.agent_env() {
        std::env::set_var(key, value);
    }
    // Tools run relative to the process's working directory, so the workspace
    // has to be it. Episodes are sequential for this reason.
    let previous = std::env::current_dir()?;
    std::env::set_current_dir(episode.workspace())?;

    let result = run_agent(family, options).await;

    std::env::set_current_dir(previous)?;
    // Stop the world before verifying: nothing the verifier reads should still
    // be able to change.
    episode.stop();

    let activity = result?;
    let predicates = run_verifier(family, &episode, &before).map_err(|e| anyhow::anyhow!("{e}"))?;
    let verdict = family
        .predicates()
        .evaluate(&predicates)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut detail = format!("{} tool call(s)", activity.calls);
    if !verdict.solved() {
        detail.push_str(&format!(", unmet: {}", verdict.failed().join(", ")));
    }
    if !activity.failures.is_empty() {
        detail.push_str(&format!(
            "; {} failed: {}",
            activity.failures.len(),
            activity.failures.join(" | ")
        ));
    }
    Ok((verdict, detail))
}

/// Give the agent the request and let its tool loop run. Returns how many tool
/// calls it made, counted from the kernel's own event stream rather than from
/// anything the model reports about itself.
async fn run_agent(family: &Family, options: &BaselineOptions) -> anyhow::Result<ToolActivity> {
    // `SVEN_MODEL` is an argument of the `sven` binary, not something the
    // SDK's loader reads, and the auto-detected default is a sentinel that
    // only resolves when exactly one chat model is served. An experiment has
    // to name its own weights regardless.
    let mut settings = config::load(None)?;
    settings.model.provider = options.model.provider().to_string();
    settings.model.name = options.model.api_model().to_string();
    if let Some(url) = &options.base_url {
        settings.model.base_url = Some(url.clone());
    }
    if let Some(key) = &options.api_key {
        settings.model.api_key = Some(key.clone());
    }

    let engine = Engine::builder()
        .config(settings)
        .approvals(ApprovalPolicy::AutoApprove)
        .build()
        .map_err(|e| anyhow::anyhow!("building the engine: {e}"))?;

    let mut agent = engine.agent("agent");
    let mut events = agent.events();
    // Counting calls is not enough to interpret an episode. A call that fails
    // because the model got the arguments wrong and a call that fails because
    // the harness is broken are the same number and opposite conclusions, and
    // the audit ledger records that a tool failed without recording why.
    let counter = tokio::spawn(async move {
        let mut activity = ToolActivity::default();
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::ToolCallStarted(_) => activity.calls += 1,
                SessionEvent::ToolCallFinished {
                    tool_name,
                    output,
                    is_error,
                    ..
                } => {
                    if is_error {
                        activity
                            .failures
                            .push(format!("{tool_name}: {}", first_line(&output)));
                    }
                }
                SessionEvent::TurnComplete | SessionEvent::Aborted { .. } => break,
                _ => {}
            }
        }
        activity
    });

    let request = match &options.hint {
        Some(hint) => format!("{}\n\n{hint}", family.request()),
        None => family.request().to_string(),
    };
    let sent = agent.send(&request).await;
    let activity = counter.await.unwrap_or_default();
    sent.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(activity)
}

/// What the agent's tools actually did, as the kernel reported it.
#[derive(Clone, Debug, Default)]
struct ToolActivity {
    calls: usize,
    /// One entry per failed call: the tool and the first line of what it
    /// said. Kept because "11 tool calls and nothing changed" is not a
    /// diagnosis, and the difference between a model that cannot form a call
    /// and a harness that cannot serve one is the whole question.
    failures: Vec<String>,
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}
