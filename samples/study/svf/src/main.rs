// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Does reading a document make an agent able to do something it could not do
//! before - and does that ability survive the document going away?
//!
//! This runs one arm of that experiment. The arm is a pair: which documents are
//! in the agent's workspace (`--errata`), and which model the engine is
//! configured to reach (ordinary sven configuration). The harness never
//! decides the second, so the same binary produces every arm:
//!
//! | Arm | `--errata` | model | must |
//! |-----|-----------|-------|------|
//! | A0  | `none`    | base  | fail - or the task is guessable and proves nothing |
//! | A1  | `real`    | base  | pass - or the document is insufficient and nothing downstream is interpretable |
//! | A2  | `none`    | trained on the errata | *the measurement* |
//! | A3  | `none`    | trained on the decoy  | not improve on A0 |
//!
//! The score is out of twenty in three tiers, each needing strictly more of
//! the errata than the one above it, so it says which piece of knowledge
//! landed rather than only how much.
//!
//! Swedish Embedded AB builds the evidence machinery that decides whether a
//! model learned something or merely moved - frozen answer sets, arms that can
//! fail, and hygiene checks that make a positive result mean what it says. If
//! your team needs expertise in evaluating self-improving systems honestly,
//! you can procure our services by sending an email to info@swedishembedded.com.

mod docs;
mod instances;
mod svf;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sven_sdk::{config, Engine};

/// Which supporting document the arm's workspace contains beside the spec.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Errata {
    /// The spec alone. The values the task needs are written down nowhere.
    None,
    /// The spec and the real errata note.
    Real,
    /// The spec and a same-shaped note about a different, irrelevant format.
    Decoy,
}

impl Errata {
    fn as_str(self) -> &'static str {
        match self {
            Errata::None => "none",
            Errata::Real => "real",
            Errata::Decoy => "decoy",
        }
    }
}

struct Args {
    errata: Errata,
    workdir: PathBuf,
    report: Option<PathBuf>,
    limit: Option<usize>,
}

const USAGE: &str = "\
sample-study-svf - run one arm of the document-to-capability experiment

    --errata none|real|decoy   which note the workspace contains (required)
    --workdir DIR              arm workspace (default target/svf-<errata>)
    --report FILE.json         machine-readable result (optional)
    --limit N                  run only the first N instances (smoke runs)
    --help
";

fn parse_args() -> Result<Args> {
    let mut errata = None;
    let mut workdir = None;
    let mut report = None;
    let mut limit = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| -> Result<String> {
            args.next()
                .with_context(|| format!("{name} needs a value\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--errata" => {
                errata = Some(match value("--errata")?.as_str() {
                    "none" => Errata::None,
                    "real" => Errata::Real,
                    "decoy" => Errata::Decoy,
                    other => bail!("unknown --errata {other:?}\n\n{USAGE}"),
                })
            }
            "--workdir" => workdir = Some(PathBuf::from(value("--workdir")?)),
            "--report" => report = Some(PathBuf::from(value("--report")?)),
            "--limit" => limit = Some(value("--limit")?.parse().context("--limit")?),
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
    }

    let errata = errata.context(format!("--errata is required\n\n{USAGE}"))?;
    Ok(Args {
        workdir: workdir
            .unwrap_or_else(|| PathBuf::from(format!("target/svf-{}", errata.as_str()))),
        errata,
        report,
        limit,
    })
}

/// Builds the arm's workspace from scratch, so a previous arm's errata - or a
/// previous run's answers - can never be what this one reads.
fn prepare_workspace(dir: &Path, errata: Errata) -> Result<Vec<String>> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).with_context(|| format!("clearing {}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let mut written = vec!["SVF.md".to_string()];
    std::fs::write(dir.join("SVF.md"), docs::SPEC)?;
    match errata {
        Errata::None => {}
        Errata::Real => {
            std::fs::write(dir.join("SVF-ERRATA.md"), docs::ERRATA)?;
            written.push("SVF-ERRATA.md".into());
        }
        Errata::Decoy => {
            std::fs::write(dir.join("QVF-ERRATA.md"), docs::DECOY)?;
            written.push("QVF-ERRATA.md".into());
        }
    }
    Ok(written)
}

/// Deletes everything in the workspace that is not one of the documents this
/// arm is supposed to contain.
///
/// Called before every instance, because the answers accumulate otherwise -
/// and an answer is a leak. Every header in this format opens with the same
/// four secret bytes, so one correct `out-header-0.json` left lying in the
/// workspace hands the signature to instance two, which would then be scored
/// as knowing something it read off the floor.
fn reset_workspace(dir: &Path, documents: &[String]) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let keep = path
            .file_name()
            .map(|n| documents.iter().any(|d| d.as_str() == n))
            .unwrap_or(false);
        if !keep && path.is_file() {
            std::fs::remove_file(&path).with_context(|| format!("clearing {}", path.display()))?;
        }
    }
    Ok(())
}

/// Every secret rendering found in the workspace, as `(file, secret)` pairs.
///
/// With the errata absent this must be empty. It is checked rather than
/// assumed because a leak here does not make the run fail - it makes the run
/// *succeed*, for the wrong reason, and look like the result we were hoping
/// for.
fn secrets_in_workspace(dir: &Path) -> Result<Vec<(String, String)>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Flattened, not merely lowercased: a note that writes the signature
        // as `C3 5A 1F 84` carries it just as completely as `c35a1f84`, and a
        // scan that only folds case would wave it through.
        let haystack = docs::flatten(&text);
        for secret in svf::secret_renderings() {
            if haystack.contains(&docs::flatten(&secret)) {
                found.push((
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into(),
                    secret,
                ));
            }
        }
    }
    Ok(found)
}

/// What happened to one instance.
///
/// `Errored` is not `Answered(false)`, and collapsing the two would be the
/// easiest way to make this experiment lie: a run that lost half its instances
/// to a network fault would report a low score indistinguishable from a model
/// that did not know the answer. The same distinction the rest of this
/// workspace draws between a verdict and `SessionOutcome::Unknown`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    /// The agent produced an answer, right or wrong.
    Answered(bool),
    /// The agent never got to answer. Not evidence about the model.
    Errored,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Answered(true) => "PASS",
            Outcome::Answered(false) => "fail",
            Outcome::Errored => "ERROR",
        }
    }
}

/// Reads one answer file and decides whether it is right.
///
/// Whitespace and case are normalised away before comparing: the experiment
/// measures whether the model knew the bytes, and failing an instance over a
/// capital letter would be measuring something else.
fn grade(path: &Path, expected: &str) -> (Outcome, String) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (Outcome::Answered(false), "no answer file".into());
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (Outcome::Answered(false), "answer file is not JSON".into());
    };
    let Some(hex) = value.get("hex").and_then(serde_json::Value::as_str) else {
        return (
            Outcome::Answered(false),
            "answer file has no string `hex`".into(),
        );
    };
    let normalised: String = hex
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if normalised == expected {
        (Outcome::Answered(true), "correct".into())
    } else if normalised.is_empty() {
        let missing = value
            .get("missing")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unspecified");
        (Outcome::Answered(false), format!("declined: {missing}"))
    } else {
        (Outcome::Answered(false), format!("wrong: {normalised}"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let report_path = args.report.as_ref().map(std::path::absolute).transpose()?;

    let workspace_files = prepare_workspace(&args.workdir, args.errata)?;
    let leaks = secrets_in_workspace(&args.workdir)?;
    if args.errata != Errata::Real && !leaks.is_empty() {
        bail!(
            "hygiene check failed: the workspace for arm --errata {} contains {} secret \
             rendering(s): {leaks:?}. A positive result from this arm would be meaningless.",
            args.errata.as_str(),
            leaks.len(),
        );
    }

    let workdir = std::path::absolute(&args.workdir)?;
    std::env::set_current_dir(&workdir)
        .with_context(|| format!("entering {}", workdir.display()))?;

    // One engine, many agents: the expensive resources are built once and the
    // twenty agents below are cheap. This is the split the framework exists
    // for, so the sample uses it rather than building an engine per task.
    let engine = Engine::builder()
        .config(config::load(None).context("loading sven configuration")?)
        // Unattended by construction (the default auto approval): a human
        // at an approval gate would be a variable this experiment cannot hold
        // constant. The workspace is a scratch directory this binary created.
        // The agents fix a failing test in a real workspace: they read, edit
        // and run it, which is exactly the coding preset.
        .toolset(sven_sdk::Toolset::coding())
        .build()
        .map_err(|e| anyhow::anyhow!("building the engine: {e}"))?;

    let mut all = instances::all();
    if let Some(limit) = args.limit {
        all.truncate(limit);
    }

    let mut results = Vec::new();
    for instance in &all {
        reset_workspace(Path::new("."), &workspace_files)?;
        // A fresh agent per instance: no conversation carries knowledge from
        // one task to the next, so twenty instances are twenty measurements
        // rather than one measurement and nineteen echoes.
        let mut agent = engine.agent("agent");
        let (outcome, reason) = match agent.send(&instance.prompt()).await {
            Ok(_) => grade(Path::new(&instance.answer_file()), &instance.expected()),
            Err(e) => (Outcome::Errored, format!("agent error: {e}")),
        };
        println!("{:<12} {:<6} {}", instance.id, outcome.label(), reason);
        results.push(serde_json::json!({
            "id": instance.id,
            "tier": instance.artifact.as_str(),
            "passed": outcome == Outcome::Answered(true),
            "errored": outcome == Outcome::Errored,
            "reason": reason,
        }));
    }

    // (passed, answered, total): the denominator that matters is `answered`,
    // and `total - answered` is how much of the run said nothing at all.
    let tier_score = |tier: &str| -> (usize, usize, usize) {
        let of_tier: Vec<_> = results.iter().filter(|r| r["tier"] == tier).collect();
        (
            of_tier.iter().filter(|r| r["passed"] == true).count(),
            of_tier.iter().filter(|r| r["errored"] == false).count(),
            of_tier.len(),
        )
    };
    let as_json = |(passed, answered, total): (usize, usize, usize)| serde_json::json!({ "passed": passed, "answered": answered, "total": total });
    let show = |tier: &str| {
        let (passed, answered, total) = tier_score(tier);
        format!(
            "{tier} {passed}/{answered}{}",
            if answered == total {
                String::new()
            } else {
                format!(" of {total}")
            }
        )
    };
    let passed = results.iter().filter(|r| r["passed"] == true).count();
    let errored = results.iter().filter(|r| r["errored"] == true).count();

    let report = serde_json::json!({
        "errata": args.errata.as_str(),
        "workspace": { "files": workspace_files, "secret_renderings_found": leaks.len() },
        "by_tier": {
            "header": as_json(tier_score("header")),
            "preamble": as_json(tier_score("preamble")),
            "frame": as_json(tier_score("frame")),
        },
        "total": {
            "passed": passed,
            "answered": results.len() - errored,
            "total": results.len(),
        },
        "instances": results,
    });

    println!(
        "\n--errata {}: {passed}/{} correct  ({}, {}, {})",
        args.errata.as_str(),
        results.len() - errored,
        show("header"),
        show("preamble"),
        show("frame"),
    );
    if errored > 0 {
        println!(
            "WARNING: {errored} of {} instances never reached the model. This arm is not \n\
             comparable with another until they are re-run.",
            results.len()
        );
    }

    if let Some(path) = report_path {
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("writing {}", path.display()))?;
        println!("report written to {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("writing a fixture answer");
        path
    }

    #[test]
    fn grading_accepts_a_correct_answer_however_it_is_spelled() {
        let dir = std::env::temp_dir().join("svf-grade-ok");
        let _ = std::fs::create_dir_all(&dir);
        let expected = svf::hex(&svf::MAGIC);

        for spelling in [
            format!(r#"{{"hex":"{expected}"}}"#),
            format!(r#"{{"hex":"{}"}}"#, expected.to_uppercase()),
            format!(r#"{{"hex":" {expected} \n"}}"#),
        ] {
            let path = answer(&dir, "a.json", &spelling);
            let (outcome, why) = grade(&path, &expected);
            assert_eq!(outcome, Outcome::Answered(true), "{spelling}: {why}");
        }
    }

    #[test]
    fn grading_separates_wrong_from_absent_from_declined() {
        let dir = std::env::temp_dir().join("svf-grade-bad");
        let _ = std::fs::create_dir_all(&dir);
        let expected = svf::hex(&svf::MAGIC);

        let cases = [
            (r#"{"hex":"deadbeef"}"#, "wrong"),
            (r#"{"hex":"","missing":"the signature"}"#, "declined"),
            (r#"{"answer":"deadbeef"}"#, "no string"),
            ("not json at all", "not JSON"),
        ];
        for (body, expect_reason) in cases {
            let path = answer(&dir, "b.json", body);
            let (outcome, why) = grade(&path, &expected);
            assert_eq!(outcome, Outcome::Answered(false), "{body}");
            assert!(why.contains(expect_reason), "{body} gave {why:?}");
        }

        let (outcome, why) = grade(&dir.join("absent.json"), &expected);
        assert_eq!(outcome, Outcome::Answered(false));
        assert!(why.contains("no answer file"), "{why}");
    }

    #[test]
    fn an_errata_free_workspace_is_clean_and_the_check_can_see_one_that_is_not() {
        // Both directions matter. A scan that never fires is not a guard, and
        // the arm it protects is the one whose result the experiment rests on.
        let base = std::env::temp_dir().join("svf-hygiene");
        let _ = std::fs::remove_dir_all(&base);

        let clean = base.join("none");
        prepare_workspace(&clean, Errata::None).expect("a workspace is prepared");
        assert!(
            secrets_in_workspace(&clean).expect("scanning").is_empty(),
            "an errata-free workspace must carry no secret"
        );

        let dirty = base.join("real");
        prepare_workspace(&dirty, Errata::Real).expect("a workspace is prepared");
        assert!(
            !secrets_in_workspace(&dirty).expect("scanning").is_empty(),
            "the scan must actually fire on a workspace that does carry one"
        );

        let decoy = base.join("decoy");
        prepare_workspace(&decoy, Errata::Decoy).expect("a workspace is prepared");
        assert!(
            secrets_in_workspace(&decoy).expect("scanning").is_empty(),
            "the decoy arm must be as secret-free as the no-errata arm"
        );
    }

    #[test]
    fn an_answer_never_survives_into_the_next_instance() {
        // Every header in this format opens with the same secret bytes, so a
        // leftover correct answer is the signature, handed to the next
        // instance for free.
        let dir = std::env::temp_dir().join("svf-reset");
        let documents = prepare_workspace(&dir, Errata::None).expect("a workspace");
        answer(&dir, "out-header-0.json", r#"{"hex":"c35a1f84010300"}"#);

        reset_workspace(&dir, &documents).expect("resetting");

        assert!(
            !dir.join("out-header-0.json").exists(),
            "the previous instance's answer survived"
        );
        assert!(
            dir.join("SVF.md").exists(),
            "resetting must not take the documents with it"
        );
    }

    #[test]
    fn preparing_a_workspace_clears_whatever_was_there_before() {
        // Arms are run one after another into paths a user picks, and an
        // errata left behind by the previous arm is the exact leak the hygiene
        // check exists to catch - better to make it impossible.
        let dir = std::env::temp_dir().join("svf-reuse");
        prepare_workspace(&dir, Errata::Real).expect("first arm");
        assert!(dir.join("SVF-ERRATA.md").exists());

        prepare_workspace(&dir, Errata::None).expect("second arm");
        assert!(
            !dir.join("SVF-ERRATA.md").exists(),
            "the previous arm's errata survived into an errata-free workspace"
        );
    }
}
