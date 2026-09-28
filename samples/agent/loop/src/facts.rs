// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents that learn from
// documents end to end with one command. If your team needs expertise in
// fine-tuning pipelines or document-to-dataset tooling, you can procure our
// services by sending an email to info@swedishembedded.com.

//! The facts pipeline in one command: document -> dataset -> adapter -> score.
//!
//! `facts` chains the four stages the sample already had - `explore`, split,
//! `train`, `eval-facts` - so learning a document needs no glue script: one
//! command reads any markdown fact sheet, extracts every fact, holds a
//! fraction of the records out for evaluation, fine-tunes the LoRA adapter
//! behind the same held-out gate `train` enforces, and scores the result on
//! the trained questions and the held-out ones. Every stage is traced and
//! every artifact lands on disk, so the report names evidence, not claims.
//!
//! The split is the part a glue script got wrong before it moved in here:
//! training needs the `{"answer": ...}` reply wrapper (it teaches the shape
//! `ask` parses) while evaluation needs the bare answer (a wrapped reference
//! is unmatchable by construction). One pool of facts, three files.

use crate::store;
use anyhow::Context;

/// One fact: the question and its reference answer.
#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) struct Fact {
    pub question: String,
    pub answer: String,
}

/// The split every stage downstream reads: training keeps the reply
/// wrapper, both evaluation sets carry the bare answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Split {
    pub train: Vec<Fact>,
    pub eval: Vec<Fact>,
}

/// Holds out every `one_in`-th record (1-based positions divisible by
/// `one_in`) for evaluation, the rest trains. A holdout of at least one
/// record whenever there is anything to hold: an evaluation set of zero
/// would read the eval stage as vacuously perfect. `one_in` below 2 is
/// refused outright - a pipeline that scored itself on its own training
/// data would report memorization as generalization.
pub(crate) fn split(facts: &[Fact], one_in: usize) -> anyhow::Result<Split> {
    anyhow::ensure!(
        one_in >= 2,
        "--holdout-one-in must be at least 2 (a holdout of nothing evaluates nothing)"
    );
    anyhow::ensure!(
        facts.len() >= one_in,
        "{} fact(s) cannot hold out 1-in-{one_in}; explore found too little",
        facts.len()
    );
    let mut train = Vec::new();
    let mut eval = Vec::new();
    for (i, fact) in facts.iter().enumerate() {
        if (i + 1) % one_in == 0 {
            eval.push(fact.clone());
        } else {
            train.push(fact.clone());
        }
    }
    Ok(Split { train, eval })
}

/// What one `facts` pipeline produced, for the summary line and the report.
#[derive(Debug)]
pub(crate) struct FactsReport {
    pub explore_run: String,
    pub facts: usize,
    pub train_records: usize,
    pub eval_records: usize,
    pub train_id: String,
    pub promoted: bool,
    pub recall_correct: usize,
    pub recall_total: usize,
    pub holdout_correct: usize,
    pub holdout_total: usize,
}

/// Reads an explore-written JSONL file into facts. The assistant side
/// carries the `{"answer": ...}` wrapper explore writes; it is unwrapped
/// here so the split decides the form, not the file.
pub(crate) fn read_facts(path: &std::path::Path) -> anyhow::Result<Vec<Fact>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut facts = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("{} line {}: not a JSON object", path.display(), n + 1))?;
        let messages = value
            .get("messages")
            .and_then(|m| m.as_array())
            .with_context(|| format!("{} line {}: no \"messages\" array", path.display(), n + 1))?;
        let content = |role: &str| -> anyhow::Result<String> {
            messages
                .iter()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some(role))
                .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                .map(str::to_string)
                .with_context(|| format!("{} line {}: no {role} message", path.display(), n + 1))
        };
        let wrapped = content("assistant")?;
        // Accept both forms: an explore file wraps, a hand-written file may
        // not. `write_split` re-wraps for training, so the wrapper on disk
        // is incidental.
        let answer =
            crate::explore::parse_answer_reply(&wrapped).unwrap_or_else(|_| wrapped.clone());
        facts.push(Fact {
            question: content("user")?,
            answer,
        });
    }
    Ok(facts)
}

/// Writes the split's three files. Each is atomic: a crash mid-pipeline
/// leaves no half-file the next stage could read as complete.
///
/// - `train_path`: the training half, replies wrapped (what teaches the
///   reply shape `ask` parses).
/// - `train_eval_path`: the SAME questions, bare answers - the recall set.
/// - `holdout_path`: the held-out questions, bare - the generalization set.
pub(crate) fn write_split(
    split: &Split,
    train_path: &std::path::Path,
    train_eval_path: &std::path::Path,
    holdout_path: &std::path::Path,
) -> anyhow::Result<()> {
    let record = |fact: &Fact, answer: serde_json::Value| -> String {
        serde_json::json!({
            "messages": [
                { "role": "user", "content": fact.question, "train": false },
                { "role": "assistant", "content": answer, "train": true },
            ],
        })
        .to_string()
    };
    let mut train_text = String::new();
    for fact in &split.train {
        let reply = serde_json::json!({ "answer": fact.answer }).to_string();
        train_text.push_str(&record(fact, serde_json::Value::String(reply)));
        train_text.push('\n');
    }
    let bare =
        |fact: &Fact| -> String { record(fact, serde_json::Value::String(fact.answer.clone())) };
    let mut recall_text = String::new();
    for fact in &split.train {
        recall_text.push_str(&bare(fact));
        recall_text.push('\n');
    }
    let mut holdout_text = String::new();
    for fact in &split.eval {
        holdout_text.push_str(&bare(fact));
        holdout_text.push('\n');
    }
    store::write_atomic(train_path, &train_text)?;
    store::write_atomic(train_eval_path, &recall_text)?;
    store::write_atomic(holdout_path, &holdout_text)?;
    Ok(())
}

/// Everything one `facts` pipeline needs.
#[derive(Clone, Debug)]
pub(crate) struct FactsOptions {
    /// The markdown fact sheet to learn from.
    pub file: Option<std::path::PathBuf>,
    /// Where the extracted dataset lives (default: under the work dir).
    /// Re-running with a populated --out reuses it instead of re-exploring.
    pub out: Option<std::path::PathBuf>,
    /// Work directory for the split, training decision and reports.
    pub work_dir: std::path::PathBuf,
    /// Every Nth record is held out of training for evaluation.
    pub holdout_one_in: usize,
    pub chunk_lines: Option<usize>,
    pub steps: u32,
    pub rank: u32,
    pub alpha: f32,
    /// Model selection, exactly as `explore` reads it.
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub local: Option<crate::provider::LocalWeights>,
}

/// One run of the whole pipeline. Stages reuse the sample's own commands -
/// explore, train, eval-facts - through their library entry points, so
/// there is one spelling of each stage and this module is composition,
/// not a second implementation.
pub(crate) fn run(options: FactsOptions) -> anyhow::Result<FactsReport> {
    let work = options.work_dir.clone();
    std::fs::create_dir_all(&work).with_context(|| format!("creating {}", work.display()))?;

    // Stage 1: extract. An existing dataset is reused (so re-running with
    // the same --out does not re-explore); otherwise explore extracts.
    let dataset = options
        .out
        .clone()
        .unwrap_or_else(|| work.join("facts.jsonl"));
    let (explore_run, facts) = if dataset.is_file() {
        eprintln!(
            "facts: reusing existing dataset {} (delete it to re-explore)",
            dataset.display()
        );
        (String::new(), read_facts(&dataset)?)
    } else {
        let file = options.file.clone().ok_or_else(|| {
            anyhow::anyhow!("facts needs --file FILE (or --out naming an existing dataset)")
        })?;
        let summary = crate::explore::run(crate::explore::ExploreOptions {
            file,
            out: dataset.clone(),
            chunk_lines: options.chunk_lines,
            model: options.model.clone(),
            base_url: options.base_url.clone(),
            api_key: options.api_key.clone(),
            local: options.local.clone(),
        })?;
        (summary.run_id.clone(), read_facts(&dataset)?)
    };
    anyhow::ensure!(!facts.is_empty(), "{} holds no facts", dataset.display());

    // Stage 2: split. Training gets the wrapper, evaluation the bare
    // answer - one pool of facts, three files.
    let split = split(&facts, options.holdout_one_in)?;
    let train_path = work.join("train.jsonl");
    let train_eval_path = work.join("train-eval.jsonl");
    let holdout_path = work.join("holdout-eval.jsonl");
    write_split(&split, &train_path, &train_eval_path, &holdout_path)?;
    eprintln!(
        "facts: split {} = {} train + {} eval (1-in-{} held out)",
        facts.len(),
        split.train.len(),
        split.eval.len(),
        options.holdout_one_in
    );

    // Stage 3: train, behind the same held-out gate `train` enforces. A
    // rejection is a finished pipeline with a negative result - the report
    // still lands, the adapter stays unserved, and the exit code says so.
    let model_dir = options
        .local
        .as_ref()
        .map(|w| w.base.clone())
        .unwrap_or_else(default_model_dir);
    let train_options = crate::train::TrainOptions {
        model_dir: model_dir.clone(),
        dataset: Some(train_path.clone()),
        steps: options.steps,
        rank: options.rank,
        alpha: options.alpha,
    };
    let (decision, train_dir) = crate::train::run(&train_options)?;
    let promoted = decision == crate::train::Decision::Promoted;
    eprintln!(
        "facts: train {} (decision record in {}/decision.json)",
        if promoted { "promoted" } else { "rejected" },
        train_dir.display()
    );

    // Stage 4: score. Recall on the trained questions, generalization on
    // the held-out ones - the two numbers a reader needs to judge what the
    // adapter actually learned. A rejected candidate is scored base-only:
    // there is no adapter to serve, and scoring a phantom would fake a
    // result.
    let serving = crate::provider::LocalWeights {
        base: model_dir,
        adapter: promoted.then(|| train_dir.join("adapter.safetensors")),
        context_tokens: options
            .local
            .as_ref()
            .map(|w| w.context_tokens)
            .unwrap_or(16_384),
    };
    let score = |name: &str, path: &std::path::Path| -> anyhow::Result<(usize, usize)> {
        let report = crate::eval::run(crate::eval::EvalOptions {
            dataset: path.to_path_buf(),
            out: work.join(format!("{name}-report.json")),
            model: options.model.clone(),
            base_url: options.base_url.clone(),
            api_key: options.api_key.clone(),
            local: Some(serving.clone()),
            shuffle: false,
            limit: None,
        })?;
        Ok((report.correct, report.total))
    };
    let (recall_correct, recall_total) = score("recall", &train_eval_path)?;
    let (holdout_correct, holdout_total) = score("holdout", &holdout_path)?;

    let report = FactsReport {
        explore_run,
        facts: facts.len(),
        train_records: split.train.len(),
        eval_records: split.eval.len(),
        train_id: train_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string(),
        promoted,
        recall_correct,
        recall_total,
        holdout_correct,
        holdout_total,
    };
    let summary = serde_json::json!({
        "schema": 1,
        "explore_run": report.explore_run,
        "facts": report.facts,
        "train_records": report.train_records,
        "eval_records": report.eval_records,
        "train_id": report.train_id,
        "promoted": report.promoted,
        "recall": { "correct": report.recall_correct, "total": report.recall_total },
        "holdout": { "correct": report.holdout_correct, "total": report.holdout_total },
    });
    store::write_atomic(
        &work.join("facts-report.json"),
        &serde_json::to_string_pretty(&summary)?,
    )?;
    Ok(report)
}

/// Where the base checkpoint lives when the caller did not say - the same
/// default every model-touching command reads.
fn default_model_dir() -> std::path::PathBuf {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => {
            std::path::PathBuf::from(home).join(".local/share/brain/models/Qwen/Qwen3-0.6B")
        }
        _ => std::path::PathBuf::from(".local/share/brain/models/Qwen/Qwen3-0.6B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(n: usize) -> Fact {
        Fact {
            question: format!("question {n}"),
            answer: format!("answer {n}"),
        }
    }

    #[test]
    fn every_nth_record_is_held_out_and_the_rest_trains() {
        let facts: Vec<Fact> = (1..=10).map(fact).collect();
        let split = split(&facts, 5).unwrap();
        assert_eq!(split.eval.len(), 2);
        assert_eq!(split.train.len(), 8);
        // The 5th and 10th records are the held-out ones.
        assert_eq!(split.eval[0].question, "question 5");
        assert_eq!(split.eval[1].question, "question 10");
        // Order is preserved in both halves: the records stay attributable
        // to the sections that produced them.
        assert_eq!(split.train[0].question, "question 1");
        assert_eq!(split.train[3].question, "question 4");
        assert_eq!(split.train[7].question, "question 9");
    }

    #[test]
    fn a_holdout_of_nothing_is_refused_not_vacuously_perfect() {
        // Fewer facts than the interval cannot hold one out.
        let err = split(&[fact(1), fact(2)], 5).unwrap_err();
        assert!(
            err.to_string().contains("too little"),
            "refusal must say the extraction was too small: {err}"
        );
        // And a degenerate interval is refused before it can produce an
        // empty evaluation set.
        assert!(split(&[fact(1)], 1).is_err());
        assert!(split(&[fact(1)], 0).is_err());
    }

    #[test]
    fn explore_written_records_read_back_as_bare_facts() {
        let dir = std::env::temp_dir().join(format!("loop-facts-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("facts.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"messages\":[{\"role\":\"user\",\"content\":\"Q1\",\"train\":false},",
                "{\"role\":\"assistant\",\"content\":\"{\\\"answer\\\":\\\"42 Mbit/s\\\"}\",",
                "\"train\":true}],\"metadata\":{\"run_id\":\"e1\"}}\n",
                // A bare reference (a hand-written file) reads the same.
                "{\"messages\":[{\"role\":\"user\",\"content\":\"Q2\",\"train\":false},",
                "{\"role\":\"assistant\",\"content\":\"3 controllers\",\"train\":true}]}\n",
            ),
        )
        .unwrap();
        let facts = read_facts(&path).unwrap();
        assert_eq!(
            facts,
            vec![
                Fact {
                    question: "Q1".into(),
                    answer: "42 Mbit/s".into()
                },
                Fact {
                    question: "Q2".into(),
                    answer: "3 controllers".into()
                },
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The split's three files carry the forms their consumers require:
    /// training wrapped, both evaluation sets bare. An evaluation file
    /// with a wrapped reference is unmatchable by construction -
    /// eval-facts refuses it - so writing the bare form here is what makes
    /// the pipeline's own scores mean anything.
    #[test]
    fn the_split_writes_wrapped_training_and_bare_evaluation() {
        let dir = std::env::temp_dir().join(format!("loop-facts-split-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let facts = vec![
            Fact {
                question: "Q1".into(),
                answer: "A1".into(),
            },
            Fact {
                question: "Q2".into(),
                answer: "A2".into(),
            },
            Fact {
                question: "Q3".into(),
                answer: "A3".into(),
            },
        ];
        let split = split(&facts, 3).unwrap();
        write_split(
            &split,
            &dir.join("train.jsonl"),
            &dir.join("train-eval.jsonl"),
            &dir.join("holdout-eval.jsonl"),
        )
        .unwrap();

        // Training: the reply is one {"answer": ...} object.
        let train = std::fs::read_to_string(dir.join("train.jsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(train.lines().next().unwrap()).unwrap();
        assert_eq!(
            v["messages"][1]["content"],
            r#"{"answer":"A1"}"#.to_string()
        );
        // Recall set: same questions as training, bare answers.
        let recall: Vec<String> = std::fs::read_to_string(dir.join("train-eval.jsonl"))
            .unwrap()
            .lines()
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                v["messages"][1]["content"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(recall, vec!["A1", "A2"]);
        // Holdout: only the held-out questions, bare.
        let holdout: Vec<String> = std::fs::read_to_string(dir.join("holdout-eval.jsonl"))
            .unwrap()
            .lines()
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                v["messages"][0]["content"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(holdout, vec!["Q3"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
