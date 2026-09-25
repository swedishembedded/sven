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
    baseline_effective, capture_path, records_from_performance, run_verifier, run_witness,
    to_jsonl, upstream_of, Action, ArmScore, Episode, Family, Outcome, Recorder, ServedModel,
};
use sven_sdk::{config, ApprovalPolicy, Engine, SessionEvent};

const USAGE: &str = "\
usage: sample-learning-tool-syntax <command> [options]

  audit                    check the task catalog: no model, no GPU, no network
  baseline [options]       measure the base model on the frozen instances
  demonstrate [options]    drive correct episodes with a scripted model and
                           emit the verified ones as training data

options for baseline:
  --instances N            episodes per twin (default 4)
  --run-dir DIR            where episodes are materialised (default: a temp dir)
  --model ID               resident manifest id (default brain/qwen3)
  --base-url URL           the served endpoint (default: sven's own configuration)
  --api-key KEY            key for that endpoint (default: BRAIN_API_KEY)
  --report FILE.json       write the machine-readable result here
  --hint TEXT              appended to the request for THIS arm only
  --prompt-from FILE       requests.json from a baseline run (demonstrate only)

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
        Some("demonstrate") => demonstrate(&args[1..]),
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
    /// A `requests.json` written by a previous `baseline` run, holding the
    /// prompt the agent really sends.
    prompt_from: Option<PathBuf>,
}

fn parse_baseline(args: &[String]) -> anyhow::Result<BaselineOptions> {
    let mut instances = 4usize;
    let mut run_dir = std::env::temp_dir().join(format!("tool-syntax-{}", std::process::id()));
    let mut model = ServedModel::qwen3();
    let mut base_url = None;
    let mut api_key = std::env::var("BRAIN_API_KEY").ok();
    let mut report = None;
    let mut hint = None;
    let mut prompt_from = None;

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
            "--prompt-from" => prompt_from = Some(PathBuf::from(take(&mut i)?)),
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
        prompt_from,
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

    // Everything the model is actually shown goes through here. An agent's
    // stored history has neither the system prompt nor the tool schemas, so
    // without this an episode cannot be turned into training data that
    // renders the way inference does.
    let recorder = match options.base_url.as_deref().and_then(upstream_of) {
        Some(upstream) => match runtime.block_on(Recorder::start(&upstream)) {
            Ok(recorder) => Some(recorder),
            Err(e) => {
                eprintln!("note: not recording model input ({e}); episodes will still run");
                None
            }
        },
        None => None,
    };
    if let Some(recorder) = &recorder {
        println!("recording model input via {}", recorder.base_url());
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
        let through = recorder.as_ref().map(|r| r.base_url());
        let outcome = runtime.block_on(one_episode(
            &family,
            &dir,
            hidden,
            &options,
            through.as_deref(),
        ));
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

    if let Some(recorder) = &recorder {
        let path = capture_path(&options.run_dir);
        if let Err(e) = runtime.block_on(recorder.dump(&path)) {
            eprintln!("note: could not write the captured model input: {e}");
        } else {
            println!("model input: {}", path.display());
        }
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
    through: Option<&str>,
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

    let result = run_agent(family, options, through).await;

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

    if let Ok(json) = serde_json::to_string_pretty(&activity.transcript) {
        let _ = std::fs::write(episode.dir().join("transcript.json"), json);
    }

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
async fn run_agent(
    family: &Family,
    options: &BaselineOptions,
    through: Option<&str>,
) -> anyhow::Result<ToolActivity> {
    // `SVEN_MODEL` is an argument of the `sven` binary, not something the
    // SDK's loader reads, and the auto-detected default is a sentinel that
    // only resolves when exactly one chat model is served. An experiment has
    // to name its own weights regardless.
    let mut settings = config::load(None)?;
    settings.model.provider = options.model.provider().to_string();
    settings.model.name = options.model.api_model().to_string();
    // Through the recorder when there is one, so the captured request is the
    // one the server really answered.
    if let Some(url) = through.or(options.base_url.as_deref()) {
        settings.model.base_url = Some(url.to_string());
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
    let mut activity = counter.await.unwrap_or_default();
    // The transcript is the episode's evidence, and it is captured whether
    // the turn succeeded or not: a failed episode is exactly the one whose
    // record explains why.
    activity.transcript = agent.state().transcript();
    sent.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(activity)
}

/// What the agent's tools actually did, as the kernel reported it.
#[derive(Clone, Debug, Default)]
struct ToolActivity {
    calls: usize,
    /// Everything the model was shown and everything it said.
    transcript: Vec<sven_sdk::Turn>,
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

// ---------------------------------------------------------------------------
// demonstrate - verified episodes from a scripted model
// ---------------------------------------------------------------------------

/// The demonstration, as decisions only.
///
/// Deliberately the plodding version: ask which deployment is live, look at
/// it, change it, check the change. A demonstration that jumped straight to
/// the right file would teach the model to guess correctly rather than to find
/// out, and finding out is the capability under test.
/// What the closing turn says once the work is done.
const CLOSING: &str =
    "The live deployment now has three retry attempts and the service validates it.";

/// The demonstration, as actions only.
///
/// Deliberately the plodding version: ask which deployment is live, look at
/// it, change it, check the change. A demonstration that jumped straight to
/// the right file would teach the model to guess correctly rather than to find
/// out, and finding out is the capability under test.
fn demonstration(family: &Family, active: &str) -> anyhow::Result<Vec<Action>> {
    let config = format!("config/{active}.json");

    // The demonstration rewrites the whole file, so it must preserve what was
    // there: `production` overrides only the timeout, and dropping that would
    // change what the deployment does - which the verifier would rightly
    // reject.
    let template = family.root().join("workspace").join(&config);
    let mut contents: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&template)?)?;
    contents
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} is not a JSON object", template.display()))?
        .insert("retry_attempts".into(), serde_json::json!(3));
    let text = format!("{}\n", serde_json::to_string_pretty(&contents)?);

    Ok(vec![
        Action::new(
            "shell",
            serde_json::json!({
                "shell_command": "./svctl status",
                "description": "find out which deployment this host runs"
            }),
        ),
        Action::new(
            "read_file",
            serde_json::json!({ "path": config, "offset": 1, "limit": 50 }),
        ),
        Action::new(
            "write_file",
            serde_json::json!({ "path": config, "text": text, "append": false }),
        ),
        Action::new(
            "shell",
            serde_json::json!({
                "shell_command": "./svctl validate",
                "description": "check the service accepts the change"
            }),
        ),
    ])
}

fn demonstrate(args: &[String]) -> anyhow::Result<()> {
    let options = parse_baseline(args)?;
    let family = Family::load(&family_root()).map_err(|e| anyhow::anyhow!("{e}"))?;

    // The prompt the agent really sends. Captured by `baseline`, because it is
    // the only place the whole thing exists - an agent's history holds neither
    // the system prompt nor the tool schemas.
    let capture = options
        .prompt_from
        .clone()
        .unwrap_or_else(|| options.run_dir.join("requests.json"));
    let captured: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&capture).map_err(|e| {
            anyhow::anyhow!(
                "{}: {e}\nRun `baseline --run-dir DIR` first; it writes the captured prompt there, \
                 or name one with --prompt-from.",
                capture.display()
            )
        })?)?;
    let prompt = captured
        .iter()
        .find(|r| {
            r.get("tools")
                .and_then(|t| t.as_array())
                .is_some_and(|t| !t.is_empty())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} holds no request carrying tool schemas; a record built from it would train the \
                 model on a prompt it never sees",
                capture.display()
            )
        })?;

    let sven = sven_binary()?;
    println!(
        "demonstrate: {} (tools via {})",
        family.id(),
        sven.display()
    );
    println!();

    let mut records = Vec::new();
    let mut solved = 0usize;
    let mut attempted = 0usize;

    for (index, hidden) in family
        .live_choices()
        .iter()
        .cycle()
        .take(options.instances * family.live_choices().len())
        .enumerate()
    {
        attempted += 1;
        let dir = options.run_dir.join(format!("demo-{index:03}-{hidden}"));
        match one_demonstration(&family, &dir, hidden, prompt, &sven) {
            Ok((verdict, produced, detail)) => {
                solved += 1;
                let _ = verdict;
                println!("  demo-{index:03} [{hidden:<10}] SOLVED  {detail}");
                records.extend(produced);
            }
            Err(e) => println!("  demo-{index:03} [{hidden:<10}] failed  {e}"),
        }
    }

    println!();
    println!(
        "demonstrate: {solved}/{attempted} verified; {} record(s)",
        records.len()
    );

    if records.is_empty() {
        anyhow::bail!(
            "no verified demonstration produced a record, so there is nothing to train on"
        );
    }

    std::fs::create_dir_all(&options.run_dir)?;
    let out = options.run_dir.join("train.jsonl");
    std::fs::write(&out, to_jsonl(&records)?)?;
    // Checked by the parser AND the encoder that will train on it, before
    // anything claims a GPU. Parsing alone is not enough: a record can satisfy
    // the wire schema completely and still have no honest loss-mask boundary
    // under the checkpoint's own chat template.
    let checked = match std::env::var("BRAIN_QWEN_WEIGHTS") {
        Ok(weights) => brain::validate_chat_dataset_for(&out, std::path::Path::new(&weights)),
        Err(_) => {
            println!(
                "note: BRAIN_QWEN_WEIGHTS is not set, so the dataset is only parsed, not encoded; \
                 a shape the template cannot mask would not be caught until training starts"
            );
            brain::validate_chat_dataset(&out)
        }
    };
    match checked {
        Ok(summary) => println!(
            "dataset: {} ({} record(s), {} message(s), {} supervised)",
            out.display(),
            summary.records,
            summary.messages,
            summary.trained_messages
        ),
        Err(e) => anyhow::bail!("the trainer would reject this dataset: {e}"),
    }
    Ok(())
}

/// Where the `sven` binary is. Named by the environment so a sample is not
/// tied to one install or one build profile.
fn sven_binary() -> anyhow::Result<PathBuf> {
    if let Ok(path) = std::env::var("SVEN_BIN") {
        return Ok(PathBuf::from(path));
    }
    let built = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|root| root.join("target/release/sven"));
    match built {
        Some(path) if path.exists() => Ok(path),
        _ => anyhow::bail!(
            "cannot find the sven binary; build it (`make release`) or name it with SVEN_BIN"
        ),
    }
}

fn one_demonstration(
    family: &Family,
    dir: &std::path::Path,
    hidden: &str,
    prompt: &serde_json::Value,
    sven: &std::path::Path,
) -> anyhow::Result<(
    sample_learning_lab::Verdict,
    Vec<sample_learning_lab::Record>,
    String,
)> {
    let mut episode = Episode::start(family, dir, hidden).map_err(|e| anyhow::anyhow!("{e}"))?;
    let before = baseline_effective(family, &episode).map_err(|e| anyhow::anyhow!("{e}"))?;

    let env = episode.agent_env();
    let actions = demonstration(family, hidden)?;
    let performed = sample_learning_lab::perform_all(sven, episode.workspace(), &env, &actions)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    episode.stop();

    let predicates = run_verifier(family, &episode, &before).map_err(|e| anyhow::anyhow!("{e}"))?;
    let verdict = family
        .predicates()
        .evaluate(&predicates)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let steps: Vec<(String, String, String)> = performed
        .iter()
        .map(|p| {
            (
                p.action.tool.clone(),
                p.action.arguments.to_string(),
                p.observation.clone(),
            )
        })
        .collect();

    let records = records_from_performance(
        family.id(),
        hidden,
        prompt,
        family.request(),
        &steps,
        CLOSING,
        &verdict,
    )
    .map_err(|why| anyhow::anyhow!("{why}"))?;

    let detail = format!(
        "{} action(s) performed, {} record(s)",
        performed.len(),
        records.len()
    );
    Ok((verdict, records, detail))
}
