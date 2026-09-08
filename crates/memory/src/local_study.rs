// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The local [`FactSubmitter`]: sven and brain on one machine, no whale.
//!
//! [`crate::drain`] left the trait deliberately generic and named no
//! implementation. The only one ever planned spoke to whale - a scale-out path
//! the product has since paused - so the drain had nothing at all to submit
//! to. This is the implementation the core learning loop actually runs: it
//! writes the batch out as brain's `{fact, probe_question, expected_answer}`
//! dataset, runs brain's gated document study as a subprocess, and turns the
//! JSON report back into one [`FactOutcome`] per fact.
//!
//! The subprocess's *shape* is what sven depends on - a dataset path in, an
//! adapter directory and a report path out - and its exact spelling is a
//! config template ([`LocalStudy::study_args`]), not a literal in this file.
//! brain owns that command and it is still moving.
//!
//! Nothing leaves the machine. That is the whole point: the config flag that
//! enables the drain is a consent boundary for facts stated in private, and
//! with this submitter behind it the facts reach a training run on the same
//! host and go no further.
//!
//! # Keeping the drain's half of the bargain
//!
//! [`FactSubmitter`] states two rules an implementation must honour or the
//! drain's exactly-once settlement is a fiction. This one honours them with a
//! journal beside the datasets:
//!
//! 1. **`submit` is durable before it returns.** Every verdict is appended to
//!    `journal.jsonl` before the reports go back to the drain, and the *claim*
//!    (which facts went into which study directory) is appended before the
//!    subprocess starts.
//! 2. **`outcomes_for` answers without re-submitting.** It reads the journal.
//!    A fact with a settled verdict is answered from it; a fact only claimed
//!    is answered from its study's `report.json` if brain got that far. No
//!    second study is ever run, whatever the answer.
//!
//! A fact with no journal entry at all is *omitted*, exactly as the trait
//! requires: silence is a positive statement that it was never accepted, and
//! it is what lets the drain send a batch a crash interrupted before it ever
//! left the process.
//!
//! # Honest limitation: an interrupted study is `Failed`, not retried
//!
//! A claim with no report means the study did not finish - the process was
//! killed, the machine went down, brain crashed. Whether it had already
//! written an adapter into the watched directory first is not knowable from
//! here, so re-submitting risks training on the fact twice, which is the one
//! thing the ledger exists to prevent. Such a fact is therefore reported
//! [`FactOutcome::Failed`]: nothing was decided, the user is told so, and it
//! is not silently sent again. Restating the fact records a new one.
//!
//! # A fact with no probe is turned down, not trained on
//!
//! Training is only defensible if something can afterwards score whether the
//! model learned the fact, and the only honest scorer is the probe frozen at
//! extraction time ([`crate::ledger::FrozenProbe`]). A fact that carries none
//! never enters the dataset and comes back [`FactOutcome::Rejected`] saying
//! exactly that - inventing a probe from the fact itself would test the
//! invention, not the knowledge.
//!
//! Swedish Embedded AB implements solutions for on-premise continuous
//! learning: knowledge capture, gated training and hot-swapped adapters that
//! never leave the customer's own hardware. If your team needs expertise in
//! closing that loop without shipping private facts to a vendor then you can
//! procure our services by sending an email to info@swedishembedded.com.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use sven_vocab::provenance::FactId;

use crate::drain::{FactOutcome, FactReport, FactSubmitter, GateNumbers};
use crate::ledger::PendingFactRecord;

/// Where the local study runs and what it runs with.
///
/// `adapter_dir` is deliberately not defaulted anywhere: it must be the very
/// directory this machine's `brain serve --watch-adapters DIR` polls, and a
/// guess that lands one directory over trains a model nothing ever serves,
/// silently and forever.
#[derive(Clone, Debug)]
pub struct LocalStudy {
    /// The `brain` executable. A bare name is resolved on `PATH`, matching how
    /// this workspace already invokes `gdb`, `rg` and `cargo`.
    pub brain_bin: String,
    /// The base checkpoint the study trains a LoRA adapter over - brain's
    /// `--weights`, and the same base the served model was built from.
    pub base_weights: String,
    /// A JSONL file of `{fact, probe_question, expected_answer}` triples: the
    /// behavioural anchor suite (system-prompt adherence, refusal, tool-call
    /// format) every cycle rehearses.
    ///
    /// brain refuses a study without one, and rightly: `Regime::Sft` mixes the
    /// anchors into every cycle's draw, and without them a cycle trains on one
    /// document alone and forgets how to behave. It is the operator's suite,
    /// not sven's to invent - what a given deployment must never lose is a
    /// property of that deployment.
    pub anchors_file: PathBuf,
    /// The argument vector handed to [`Self::brain_bin`], with
    /// [`WEIGHTS_PLACEHOLDER`], [`DATASET_PLACEHOLDER`],
    /// [`ADAPTER_DIR_PLACEHOLDER`] and [`REPORT_PLACEHOLDER`] substituted.
    ///
    /// A template rather than a literal invocation because the subcommand's
    /// final spelling is brain's to decide and is still moving - the study
    /// machinery turned out to be architecture-agnostic, so it is becoming a
    /// top-level command with an `--arch` rather than living under one model's
    /// subcommand tree. What sven actually depends on is the *shape*: a
    /// dataset path in, an adapter directory and a report path out. Keeping
    /// the spelling in config makes catching up with brain a one-line edit
    /// instead of a release.
    pub study_args: Vec<String>,
    /// The directory `brain serve --watch-adapters` polls for promoted
    /// adapters. A promoted study publishes into it and the running server
    /// hot-swaps; nothing else connects the two halves.
    pub adapter_dir: PathBuf,
    /// Where datasets, reports and the outcome journal are kept. One
    /// subdirectory per study, so a report stays readable after the fact.
    pub work_dir: PathBuf,
    /// How long one study may run before it is killed.
    pub timeout: Duration,
}

/// Stands for the base checkpoint, in [`LocalStudy::study_args`].
pub const WEIGHTS_PLACEHOLDER: &str = "{weights}";
/// Stands for the dataset sven writes, in [`LocalStudy::study_args`].
pub const DATASET_PLACEHOLDER: &str = "{dataset}";
/// Stands for the directory a promoted adapter is published into.
pub const ADAPTER_DIR_PLACEHOLDER: &str = "{adapter_dir}";
/// Stands for the JSON report path sven reads back.
pub const REPORT_PLACEHOLDER: &str = "{report}";

impl LocalStudy {
    /// The invocation sven assumes when config names none.
    ///
    /// Read off brain's own `document-study` usage line. Still a default
    /// rather than a constant: brain owns that command, and the optional
    /// study knobs it also accepts (`--lora`, `--steps`, `--lr`,
    /// `--eval-per-cycle`, `--seed`, `--models-dir`) are set by replacing
    /// this list, not by growing a sven config field per brain flag.
    #[must_use]
    pub fn default_study_args() -> Vec<String> {
        [
            "document-study",
            "--arch",
            "qwen3",
            "--weights",
            WEIGHTS_PLACEHOLDER,
            "--dataset",
            DATASET_PLACEHOLDER,
            "--adapter-dir",
            ADAPTER_DIR_PLACEHOLDER,
            "--report",
            REPORT_PLACEHOLDER,
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
    }

    /// [`Self::study_args`] with the three paths substituted.
    fn argv(&self, dataset: &Path, report: &Path) -> Vec<String> {
        self.study_args
            .iter()
            .map(|arg| {
                arg.replace(WEIGHTS_PLACEHOLDER, &self.base_weights)
                    .replace(DATASET_PLACEHOLDER, &dataset.to_string_lossy())
                    .replace(ADAPTER_DIR_PLACEHOLDER, &self.adapter_dir.to_string_lossy())
                    .replace(REPORT_PLACEHOLDER, &report.to_string_lossy())
            })
            .collect()
    }

    /// How the invocation reads in an error message.
    fn described(&self) -> String {
        format!("{} {}", self.brain_bin, self.study_args.join(" "))
    }
}

/// Runs brain's gated document study on this machine, one batch at a time.
pub struct LocalFactSubmitter {
    study: LocalStudy,
}

impl LocalFactSubmitter {
    /// A submitter that runs studies as described by `study`.
    #[must_use]
    pub fn new(study: LocalStudy) -> Self {
        Self { study }
    }

    /// The append-only record of what was claimed and what was settled.
    fn journal_path(&self) -> PathBuf {
        self.study.work_dir.join("journal.jsonl")
    }

    fn append_journal(&self, entry: &JournalEntry) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.study.work_dir)?;
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.journal_path())?;
        file.write_all(&line)?;
        // Durable before the caller is told anything: a verdict the drain acted
        // on and this process then forgot is exactly the lost outcome the whole
        // protocol exists to prevent.
        file.sync_all()?;
        Ok(())
    }

    fn read_journal(&self) -> anyhow::Result<Vec<JournalEntry>> {
        let text = match std::fs::read_to_string(self.journal_path()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            out.push(serde_json::from_str(line)?);
        }
        Ok(out)
    }

    /// Runs the study over `dataset`, writing its report to `report`.
    ///
    /// A non-zero exit is not fatal on its own: brain always writes a report,
    /// and a report that exists is a better answer than the exit code. Only a
    /// failure that left no report at all is reported as an error.
    async fn run_study(&self, dataset: &Path, report: &Path) -> anyhow::Result<()> {
        let mut cmd = tokio::process::Command::new(&self.study.brain_bin);
        cmd.args(self.study.argv(dataset, report))
            // The study owns the machine's GPU for minutes; a killed sven must
            // not leave it running against a directory nobody is watching.
            .kill_on_drop(true);
        let output = match tokio::time::timeout(self.study.timeout, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                anyhow::bail!(
                    "could not run `{}`: {e}. Set tools.memory.learning.brain_bin if brain \
                     is not on PATH, and study_args if the subcommand has moved.",
                    self.study.described()
                )
            }
            Err(_) => anyhow::bail!(
                "`{}` did not finish within {}s",
                self.study.described(),
                self.study.timeout.as_secs()
            ),
        };
        if !output.status.success() && !report.is_file() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim();
            // The tail, not the head: a training run's last words are the ones
            // that say why it died.
            let tail: String = stderr
                .char_indices()
                .rev()
                .take(400)
                .last()
                .map(|(i, _)| &stderr[i..])
                .unwrap_or_default()
                .to_string();
            anyhow::bail!(
                "`{}` failed ({}) and wrote no report: {tail}",
                self.study.described(),
                output.status,
            );
        }
        Ok(())
    }
}

#[async_trait]
impl FactSubmitter for LocalFactSubmitter {
    async fn submit(&self, batch: &[PendingFactRecord]) -> anyhow::Result<Vec<FactReport>> {
        let scoreable: Vec<&PendingFactRecord> =
            batch.iter().filter(|f| f.probe.is_some()).collect();

        let report = if scoreable.is_empty() {
            None
        } else {
            let dir = self
                .study
                .work_dir
                .join(format!("study-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&dir)?;
            let dataset = dir.join("dataset.json");
            std::fs::write(
                &dataset,
                dataset_bytes(&scoreable, &read_anchors(&self.study.anchors_file)?)?,
            )?;

            // Durable before the subprocess starts, so a crash mid-study is
            // recoverable through `outcomes_for` rather than invisible.
            self.append_journal(&JournalEntry::Claimed {
                study: dir.clone(),
                facts: scoreable
                    .iter()
                    .map(|f| ClaimedFact {
                        id: f.id.clone(),
                        fact: f.fact.clone(),
                    })
                    .collect(),
            })?;

            let report_path = dir.join("report.json");
            self.run_study(&dataset, &report_path).await?;
            Some(read_report(&report_path)?)
        };

        let reports: Vec<FactReport> = batch
            .iter()
            .map(|fact| FactReport {
                id: fact.id.clone(),
                outcome: match (&fact.probe, &report) {
                    (None, _) => unscoreable(),
                    (Some(_), Some(report)) => report.outcome_for(&fact.fact),
                    // Unreachable in practice - a fact with a probe means the
                    // study ran - but it is a verdict, not a panic.
                    (Some(_), None) => FactOutcome::Failed {
                        reason: "no document study was run for this fact".to_string(),
                    },
                },
            })
            .collect();
        for report in &reports {
            self.append_journal(&JournalEntry::Settled {
                id: report.id.clone(),
                outcome: report.outcome.clone(),
            })?;
        }
        Ok(reports)
    }

    async fn outcomes_for(&self, facts: &[FactId]) -> anyhow::Result<Vec<FactReport>> {
        let entries = self.read_journal()?;
        let mut settled: HashMap<&FactId, &FactOutcome> = HashMap::new();
        let mut claimed: HashMap<&FactId, (&Path, &str)> = HashMap::new();
        for entry in &entries {
            match entry {
                JournalEntry::Claimed { study, facts } => {
                    for fact in facts {
                        claimed.insert(&fact.id, (study.as_path(), fact.fact.as_str()));
                    }
                }
                JournalEntry::Settled { id, outcome } => {
                    settled.insert(id, outcome);
                }
            }
        }

        let mut out = Vec::new();
        let mut newly_settled = Vec::new();
        for id in facts {
            let outcome = if let Some(outcome) = settled.get(id) {
                (*outcome).clone()
            } else if let Some((study, fact)) = claimed.get(id) {
                // The study ran but this process never recorded its verdict.
                // brain's report is the durable half of that, when it exists.
                let outcome = match read_report(&study.join("report.json")) {
                    Ok(report) => report.outcome_for(fact),
                    Err(e) => FactOutcome::Failed {
                        reason: format!(
                            "the document study was interrupted before it reported: {e}. \
                             It is not re-run automatically, because whether it had already \
                             trained on this fact is not knowable from here."
                        ),
                    },
                };
                newly_settled.push(FactReport {
                    id: id.clone(),
                    outcome: outcome.clone(),
                });
                outcome
            } else {
                // Never accepted: say nothing at all, so the drain knows it is
                // safe to submit after all.
                continue;
            };
            out.push(FactReport {
                id: id.clone(),
                outcome,
            });
        }
        for report in &newly_settled {
            self.append_journal(&JournalEntry::Settled {
                id: report.id.clone(),
                outcome: report.outcome.clone(),
            })?;
        }
        Ok(out)
    }
}

/// The verdict for a fact nothing can score.
fn unscoreable() -> FactOutcome {
    FactOutcome::Rejected {
        reason: "no frozen probe was captured with this fact, so no study can decide \
                 whether the model learned it"
            .to_string(),
        numbers: GateNumbers::default(),
    }
}

/// brain's dataset: one JSON object carrying the study's cycles and the
/// behavioural anchor suite every cycle rehearses.
///
/// brain decodes it with `deny_unknown_fields` and no optional members, so an
/// extra key or an absent one is a loud, named parse failure there rather than
/// a plausible default that trains on the wrong thing. That is deliberate on
/// brain's side and it is why this writes exactly the fields it decodes, and
/// no identifier - the report is matched back by fact text.
///
/// One batch is one cycle. A drain pass hands over the facts that accumulated
/// since the last one, and "these facts were learned together" is what a cycle
/// means.
fn dataset_bytes(facts: &[&PendingFactRecord], anchors: &[Triple]) -> anyhow::Result<Vec<u8>> {
    let cycle: Vec<Triple> = facts
        .iter()
        .filter_map(|fact| {
            fact.probe.as_ref().map(|probe| Triple {
                fact: fact.fact.clone(),
                probe_question: probe.question.clone(),
                expected_answer: probe.expected_answer.clone(),
            })
        })
        .collect();
    Ok(serde_json::to_vec(&serde_json::json!({
        "cycles": [cycle],
        "anchors": anchors,
    }))?)
}

/// One `{fact, probe_question, expected_answer}` triple, brain's `FactProbe`.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Triple {
    fact: String,
    probe_question: String,
    expected_answer: String,
}

/// The operator's anchor suite, one triple per non-empty line.
///
/// Read on every submission rather than cached: an operator who adds an anchor
/// after noticing a regression should not have to restart sven for the next
/// study to rehearse it.
fn read_anchors(path: &Path) -> anyhow::Result<Vec<Triple>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(
            "tools.memory.learning.anchors_file {}: {e}. It must be a JSONL file of \
             {{fact, probe_question, expected_answer}} triples - the behaviours every study \
             has to preserve. brain refuses a study without one.",
            path.display()
        )
    })?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line).map_err(|e| {
            anyhow::anyhow!("{}: line {}: {e}", path.display(), i + 1)
        })?);
    }
    if out.is_empty() {
        anyhow::bail!(
            "tools.memory.learning.anchors_file {} is empty. Regime::Sft mixes the anchor \
             suite into every cycle's draw, so without it a cycle trains on one document \
             alone and forgets how to behave.",
            path.display()
        );
    }
    Ok(out)
}

fn read_report(path: &Path) -> anyhow::Result<StudyReport> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("no study report at {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("study report at {} is malformed: {e}", path.display()))
}

/// What brain's `document-study --report PATH` writes.
///
/// Every field is `#[serde(default)]` and unknown ones are ignored: brain owns
/// this format and will grow it, and a sven that refused to parse a report
/// because brain added a field would take the whole loop down for a cosmetic
/// change. What sven actually needs is the promote/reject decision and, per
/// cycle, which facts that cycle was about.
#[derive(Debug, Default, Deserialize)]
struct StudyReport {
    /// Whether the gate promoted at least one cycle, which is exactly when an
    /// adapter was published into the watched directory.
    #[serde(default)]
    promoted: bool,
    /// The real gate's arm. The `null_gate` arm beside it is the control that
    /// licenses believing this one; it says nothing about a specific fact and
    /// is deliberately not read here.
    #[serde(default)]
    gated: ArmReport,
}

#[derive(Debug, Default, Deserialize)]
struct ArmReport {
    #[serde(default)]
    cycles: Vec<CycleRow>,
}

/// One cycle of the study: the facts it trained on, and what the gate said.
///
/// Note what is NOT here: a per-fact pass rate. brain reports per-CYCLE
/// numbers because the gate's decision is a per-cycle measurement, and
/// reconstructing per-fact verdicts would need a second decode pass - a
/// different measurement from the gate's, presented as if it were the gate's.
/// So a fact's verdict is its cycle's verdict, and the reason sven reports
/// carries the cycle's own numbers rather than inventing per-fact ones.
#[derive(Debug, Default, Deserialize)]
struct CycleRow {
    /// The distinct fact statements this cycle trained on.
    #[serde(default)]
    facts: Vec<String>,
    /// The incumbent arm on this cycle's frozen probes.
    #[serde(default)]
    baseline_pass_rate: Option<f64>,
    /// The candidate arm on the same probes, after training.
    #[serde(default)]
    post_training_pass_rate: Option<f64>,
    /// `"promote"` or `"reject"`.
    #[serde(default)]
    decision: String,
    /// Which of the gate's checks rejected, with the numbers that decided it.
    #[serde(default)]
    reject_cause: Option<String>,
    /// The gate's sign-test p-value for this cycle.
    #[serde(default)]
    p_value: Option<f64>,
    /// The gate's effect size for this cycle.
    #[serde(default)]
    effect_size: Option<f64>,
}

impl CycleRow {
    /// This cycle's evidence, in the machine-readable shape
    /// [`FactOutcome::Promoted`]/[`FactOutcome::Rejected`] carry.
    fn numbers(&self) -> GateNumbers {
        GateNumbers {
            baseline_pass_rate: self.baseline_pass_rate,
            post_training_pass_rate: self.post_training_pass_rate,
            p_value: self.p_value,
            effect_size: self.effect_size,
        }
    }
}

impl StudyReport {
    /// This report's verdict on one submitted fact.
    ///
    /// Matched by normalised fact text, because brain's dataset format carries
    /// no identifier to match on and brain itself collapses training rows
    /// under exactly this identity.
    fn outcome_for(&self, fact: &str) -> FactOutcome {
        let identity = normalized(fact);
        let Some(cycle) = self
            .gated
            .cycles
            .iter()
            .find(|c| c.facts.iter().any(|f| normalized(f) == identity))
        else {
            // Two different failures read alike from one fact's point of
            // view, but a human diagnosing them needs to tell them apart: a
            // report with SOME cycles that simply doesn't mention this fact
            // is (per the doc comment above) unreachable in practice: a
            // wholly EMPTY report is far more likely brain's shape having
            // moved under a field this decoder still reads by the old name
            // - `#[serde(default)]` tolerates a genuinely new field, not a
            // renamed one it depended on.
            let reason = if self.gated.cycles.is_empty() {
                "the study report has no cycles at all - either the study trained on \
                 nothing, or brain's report format no longer matches what this decoder \
                 reads (check for a renamed field, not just a new one)"
                    .to_string()
            } else {
                "the study report's cycles do not mention this fact".to_string()
            };
            return FactOutcome::Failed { reason };
        };
        let numbers = cycle.numbers();
        // Both halves are required: a cycle the gate promoted still trained
        // nothing servable if no adapter was published.
        if self.promoted && cycle.decision == "promote" {
            return FactOutcome::Promoted { numbers };
        }
        FactOutcome::Rejected {
            reason: format!(
                "the gate did not promote the study this fact was learned in{}{}",
                cycle
                    .reject_cause
                    .as_deref()
                    .map(|c| format!(": {c}"))
                    .unwrap_or_default(),
                cycle.rates()
            ),
            numbers,
        }
    }
}

impl CycleRow {
    /// ` (probe pass rate: baseline X, after training Y)`, or nothing when the
    /// report carried no rates.
    fn rates(&self) -> String {
        match (self.baseline_pass_rate, self.post_training_pass_rate) {
            (Some(base), Some(trained)) => {
                format!(" (probe pass rate: baseline {base}, after training {trained})")
            }
            (None, Some(trained)) => format!(" (probe pass rate after training: {trained})"),
            _ => String::new(),
        }
    }
}

/// Case- and whitespace-insensitive text, matching brain's own `normalize`.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
        .to_lowercase()
}

/// One line of the durable record behind `outcomes_for`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JournalEntry {
    /// Written *before* the study starts: these facts are in that directory's
    /// dataset, and whatever happens next, they were handed over.
    Claimed {
        study: PathBuf,
        facts: Vec<ClaimedFact>,
    },
    /// Written before the verdict is returned to the drain.
    Settled { id: FactId, outcome: FactOutcome },
}

/// A claimed fact, with the text the report is matched back by.
#[derive(Debug, Serialize, Deserialize)]
struct ClaimedFact {
    id: FactId,
    fact: String,
}

/// The submitter `config` selects, or `None` when it selects none.
///
/// The selection is a config value rather than a compile-time choice so a
/// later whale-backed submitter can be swapped in without the drain - which is
/// generic over [`FactSubmitter`] precisely so this stays possible - changing
/// at all.
///
/// # Errors
///
/// An unknown submitter name, or `local` without one of the three paths it
/// cannot guess: where the promoted adapter must be published, which base
/// checkpoint to train over, and which behaviours every study must preserve.
pub fn submitter_from_config(
    config: &sven_config::Config,
) -> anyhow::Result<Option<Arc<dyn FactSubmitter>>> {
    let learning = &config.tools.memory.learning;
    match learning.submitter.as_str() {
        "none" => Ok(None),
        "local" => {
            let adapter_dir = learning.adapter_dir.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "tools.memory.learning.adapter_dir is not set. It must be the same \
                     directory this machine's `brain serve --watch-adapters DIR` polls - \
                     sven cannot guess it, and a wrong one trains a model nothing serves."
                )
            })?;
            let base_weights = learning.base_weights.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "tools.memory.learning.base_weights is not set. The study trains a LoRA \
                     adapter OVER a base checkpoint, and it has to be the base the served \
                     model was built from - an adapter trained over a different one is not \
                     applicable to what is running."
                )
            })?;
            let anchors_file = learning.anchors_file.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "tools.memory.learning.anchors_file is not set. It is the behavioural \
                     anchor suite every study rehearses, and brain refuses a study without \
                     one: what this deployment must never forget is the operator's to state, \
                     not sven's to invent."
                )
            })?;
            Ok(Some(Arc::new(LocalFactSubmitter::new(LocalStudy {
                brain_bin: learning.brain_bin.clone(),
                base_weights,
                anchors_file: expand(anchors_file),
                study_args: learning
                    .study_args
                    .clone()
                    .unwrap_or_else(LocalStudy::default_study_args),
                adapter_dir: expand(adapter_dir),
                work_dir: learning
                    .work_dir
                    .as_deref()
                    .map(expand)
                    .unwrap_or_else(default_work_dir),
                timeout: Duration::from_secs(learning.study_timeout_secs.max(1)),
            }))))
        }
        other => anyhow::bail!(
            "unknown tools.memory.learning.submitter {other:?}: expected \"local\" or \"none\""
        ),
    }
}

/// Datasets, reports and the journal, beside the ledger they come from.
fn default_work_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/sven/memory/learning")
}

/// `~` and `$VAR` in a configured path, the same latitude every other path in
/// this config gets.
fn expand(path: &str) -> PathBuf {
    PathBuf::from(shellexpand::full(path).map_or_else(|_| path.to_string(), |s| s.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(json: &str) -> StudyReport {
        serde_json::from_str(json).expect("report decodes")
    }

    /// brain owns the report format and will grow it - it already carries a
    /// whole null-gate arm, per-cycle gate statistics and a retention matrix
    /// sven never reads. A sven that refused to parse a report over a field it
    /// never asked for would take the loop down for a cosmetic change on the
    /// other side.
    #[test]
    fn a_report_carrying_fields_sven_never_asked_for_still_decodes() {
        let r = report(
            r#"{"arch": "qwen3", "base": "qwen3-0.6b", "cycles": 1, "preregistered": true,
                "baseline_untrained": 0.02, "arm_separation": 0.31, "decision": "promote",
                "promoted": true, "adapter": "adapter-000003.safetensors",
                "gated": {"acc": 0.9, "bwt": -0.01, "promotions": 1, "cycles": [
                  {"cycle": 0, "label": "doc", "facts": ["The bus runs at 500 kbit/s."],
                   "baseline_pass_rate": 0.0, "post_training_pass_rate": 1.0,
                   "decision": "promote", "reject_cause": null, "applied_promote": true,
                   "p_value": 0.001, "effect_size": 1.0, "n_discordant": 12, "k_wins": 12,
                   "anchor_delta": 0.0, "entropy_ratio": 0.98, "retention_row": [1.0]}]},
                "null_gate": {"acc": 0.5, "bwt": 0.0, "promotions": 0, "cycles": []}}"#,
        );
        assert_eq!(
            r.outcome_for("the bus runs at   500 KBIT/S."),
            FactOutcome::Promoted {
                numbers: GateNumbers {
                    baseline_pass_rate: Some(0.0),
                    post_training_pass_rate: Some(1.0),
                    p_value: Some(0.001),
                    effect_size: Some(1.0),
                }
            },
            "a fact is matched by normalised text, exactly as brain collapses its own rows, \
             and the gate's own numbers ride along with the verdict"
        );
    }

    /// A study whose gate rejected published no adapter, so nothing in it
    /// reached the served model - and the user is owed the gate's own reason
    /// rather than a bare "no".
    #[test]
    fn a_rejected_cycle_rejects_its_facts_and_carries_the_gates_reason() {
        let r = report(
            r#"{"promoted": false, "decision": "reject",
                "gated": {"cycles": [{"facts": ["The bus runs at 500 kbit/s."],
                  "baseline_pass_rate": 0.0, "post_training_pass_rate": 0.33,
                  "decision": "reject", "reject_cause": "anchor suite regressed by 0.08"}]}}"#,
        );
        assert!(
            matches!(r.outcome_for("The bus runs at 500 kbit/s."),
                     FactOutcome::Rejected { reason, .. }
                     if reason.contains("anchor suite regressed") && reason.contains("0.33")),
            "the cause and the cycle's own numbers must both reach the user"
        );
    }

    /// A wholly empty report is far more likely a broken/renamed field on
    /// brain's side than a real study that trained on nothing - `outcome_for`
    /// must say so distinctly, not blend it with the "this one fact wasn't
    /// mentioned" case below.
    #[test]
    fn an_empty_report_is_failed_with_a_reason_naming_the_report_not_the_fact() {
        let r = report(r#"{"promoted": true, "gated": {"cycles": []}}"#);
        assert!(
            matches!(r.outcome_for("The bus runs at 500 kbit/s."),
                FactOutcome::Failed { reason } if reason.contains("no cycles at all")),
            "an empty report must be distinguishable from a fact simply absent \
             from an otherwise-populated one"
        );
    }

    /// The report is the only thing that can settle a fact. A fact brain never
    /// mentioned - among cycles that DO exist - has no verdict, and inventing
    /// one either way would be a lie the user acts on.
    #[test]
    fn a_fact_absent_from_a_nonempty_report_is_failed_never_guessed_at() {
        let r = report(
            r#"{"promoted": true, "gated": {"cycles": [
                 {"facts": ["A wholly different fact."], "decision": "promote"}]}}"#,
        );
        assert!(matches!(
            r.outcome_for("The bus runs at 500 kbit/s."),
            FactOutcome::Failed { .. }
        ));
    }

    /// The anchor suite is the operator's, and brain refuses a study without
    /// one - so an unreadable or empty file is a named error naming the config
    /// key, not a study that quietly rehearses nothing.
    #[test]
    fn an_absent_or_empty_anchor_suite_is_refused_by_name() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let missing = read_anchors(&dir.path().join("nope.jsonl")).expect_err("absent");
        assert!(missing.to_string().contains("anchors_file"));

        let empty = dir.path().join("empty.jsonl");
        std::fs::write(&empty, "\n\n").expect("write");
        assert!(read_anchors(&empty)
            .expect_err("empty")
            .to_string()
            .contains("is empty"));
    }
}
