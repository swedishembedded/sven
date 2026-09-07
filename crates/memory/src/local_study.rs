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

use crate::drain::{FactOutcome, FactReport, FactSubmitter};
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
    /// The argument vector handed to [`Self::brain_bin`], with
    /// [`DATASET_PLACEHOLDER`], [`ADAPTER_DIR_PLACEHOLDER`] and
    /// [`REPORT_PLACEHOLDER`] substituted.
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

/// Stands for the dataset sven writes, in [`LocalStudy::study_args`].
pub const DATASET_PLACEHOLDER: &str = "{dataset}";
/// Stands for the directory a promoted adapter is published into.
pub const ADAPTER_DIR_PLACEHOLDER: &str = "{adapter_dir}";
/// Stands for the JSON report path sven reads back.
pub const REPORT_PLACEHOLDER: &str = "{report}";

impl LocalStudy {
    /// The invocation sven assumes when config names none.
    ///
    /// **Unverified against a landed brain command.** It is sven's best
    /// reading of the interface brain is building - a top-level,
    /// architecture-agnostic `document-study` following brain's existing
    /// `bench eval --arch <name>` pattern - and it is a default rather than a
    /// constant precisely so that being wrong costs one config line.
    #[must_use]
    pub fn default_study_args() -> Vec<String> {
        [
            "document-study",
            "--arch",
            "qwen3",
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
                arg.replace(DATASET_PLACEHOLDER, &dataset.to_string_lossy())
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
            let dataset = dir.join("dataset.jsonl");
            std::fs::write(&dataset, dataset_bytes(&scoreable)?)?;

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
    }
}

/// brain's dataset: one `{fact, probe_question, expected_answer}` object per
/// line.
///
/// brain decodes it into a `FactProbe` with `deny_unknown_fields` and no
/// optional members, so an extra key here is a loud parse failure there. That
/// is deliberate on brain's side and it is why this writes exactly three
/// fields and no identifier - the report is matched back by fact text.
fn dataset_bytes(facts: &[&PendingFactRecord]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    for fact in facts {
        let Some(probe) = &fact.probe else { continue };
        serde_json::to_writer(
            &mut out,
            &serde_json::json!({
                "fact": fact.fact,
                "probe_question": probe.question,
                "expected_answer": probe.expected_answer,
            }),
        )?;
        out.push(b'\n');
    }
    Ok(out)
}

fn read_report(path: &Path) -> anyhow::Result<StudyReport> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("no study report at {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("study report at {} is malformed: {e}", path.display()))
}

/// What `brain qwen3 document-study --report PATH` writes.
///
/// Every field is `#[serde(default)]` and unknown ones are ignored: brain owns
/// this format and will grow it (gate statistics, the null-gate arm, the
/// retention matrix), and a sven that refuses to parse a report because brain
/// added a field would take the whole loop down for a cosmetic change. What
/// sven actually needs is the promote/reject decision and the per-fact rows.
#[derive(Debug, Default, Deserialize)]
struct StudyReport {
    /// Whether the candidate adapter cleared the pre-registered gate and was
    /// published into the watched directory.
    #[serde(default)]
    promoted: bool,
    /// Why the gate turned the study down, when it did.
    #[serde(default)]
    reason: Option<String>,
    /// One row per distinct fact in the dataset.
    #[serde(default)]
    facts: Vec<ReportedFact>,
}

/// One fact's row in the report: brain's own `FactVerdict`, plus the two rates
/// that make the verdict legible to a human.
#[derive(Debug, Default, Deserialize)]
struct ReportedFact {
    #[serde(default)]
    fact: String,
    /// `true` iff every one of this fact's frozen probes passed on the
    /// candidate arm. Not a mean - a fact with one failing probe has not
    /// landed.
    #[serde(default)]
    landed: bool,
    #[serde(default)]
    baseline_pass_rate: Option<f64>,
    #[serde(default)]
    trained_pass_rate: Option<f64>,
}

impl StudyReport {
    /// This report's verdict on one submitted fact.
    ///
    /// Matched by normalised fact text, because brain's dataset format carries
    /// no identifier to match on and brain itself collapses training rows under
    /// exactly this identity.
    fn outcome_for(&self, fact: &str) -> FactOutcome {
        let identity = normalized(fact);
        let Some(row) = self.facts.iter().find(|r| normalized(&r.fact) == identity) else {
            return FactOutcome::Failed {
                reason: "the study report said nothing about this fact".to_string(),
            };
        };
        if !self.promoted {
            return FactOutcome::Rejected {
                reason: format!(
                    "the study did not clear its gate, so no adapter was published{}{}",
                    self.reason
                        .as_deref()
                        .map(|r| format!(": {r}"))
                        .unwrap_or_default(),
                    row.rates()
                ),
            };
        }
        if row.landed {
            return FactOutcome::Promoted;
        }
        FactOutcome::Rejected {
            reason: format!(
                "the adapter was promoted, but this fact's own frozen probes still fail{}",
                row.rates()
            ),
        }
    }
}

impl ReportedFact {
    /// ` (probe pass rate: baseline X, trained Y)`, or nothing when the report
    /// carried no rates.
    fn rates(&self) -> String {
        match (self.baseline_pass_rate, self.trained_pass_rate) {
            (Some(base), Some(trained)) => {
                format!(" (probe pass rate: baseline {base}, trained {trained})")
            }
            (None, Some(trained)) => format!(" (probe pass rate: trained {trained})"),
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
/// An unknown submitter name, or `local` without the `adapter_dir` it cannot
/// guess.
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
            Ok(Some(Arc::new(LocalFactSubmitter::new(LocalStudy {
                brain_bin: learning.brain_bin.clone(),
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

    /// brain owns the report format and will grow it. A sven that refused to
    /// parse a report over a field it never asked for would take the loop down
    /// for a cosmetic change on the other side.
    #[test]
    fn a_report_carrying_fields_sven_never_asked_for_still_decodes() {
        let r = report(
            r#"{"promoted": true, "gate": {"n": 60, "p_value": 0.001},
                "null_gate": {"decision": "reject"}, "bwt": -0.02,
                "facts": [{"fact": "f", "landed": true, "probes": 3}]}"#,
        );
        assert!(r.promoted);
        assert_eq!(r.facts.len(), 1);
    }

    /// A study that failed its gate published no adapter, so nothing in it
    /// landed - regardless of what the per-fact rows say about probe scores.
    #[test]
    fn a_rejected_study_rejects_every_fact_in_it_even_the_ones_whose_probes_passed() {
        let r = report(
            r#"{"promoted": false, "reason": "the null-gate arm fired",
                "facts": [{"fact": "The bus runs at 500 kbit/s.", "landed": true}]}"#,
        );
        assert!(
            matches!(r.outcome_for("the bus runs at   500 KBIT/S."),
                     FactOutcome::Rejected { reason } if reason.contains("null-gate")),
            "a fact is matched by normalised text, and a failed gate is the reason"
        );
    }

    /// The report is the only thing that can settle a fact. A fact brain never
    /// mentioned has no verdict, and inventing one either way would be a lie
    /// the user acts on.
    #[test]
    fn a_fact_the_report_never_mentions_is_failed_never_guessed_at() {
        let r = report(r#"{"promoted": true, "facts": []}"#);
        assert!(matches!(
            r.outcome_for("The bus runs at 500 kbit/s."),
            FactOutcome::Failed { .. }
        ));
    }
}
