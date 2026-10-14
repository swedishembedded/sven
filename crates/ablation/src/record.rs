// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The raw record of one run: the stable, versioned file the report and the
//! fixture exporter read. Everything the report says is derived from these.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sven_vocab::LedgerSummary;

use crate::matrix::{Role, Variant};
use crate::score::Score;
use crate::task::Suite;

/// The only run-record format this build reads and writes.
pub const RUN_SCHEMA: &str = "sven.ablation.run/1";

/// Printed in every report header and carried by every record: what a
/// scripted run does and does not establish.
pub const SCRIPTED_NOTICE: &str = "SCRIPTED RUNS: the model is a fixed script, so these runs \
measure what the harness does with a given model behaviour (tokens by source, evidence kept, \
shortlists, restarts, security). They do not measure whether a model completes tasks, and no \
task-success claim follows from them.";

/// The variant a run belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantRef {
    pub id: String,
    pub name: String,
    pub role: Role,
}

impl From<&Variant> for VariantRef {
    fn from(v: &Variant) -> Self {
        Self {
            id: v.id.clone(),
            name: v.name.clone(),
            role: v.role,
        }
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunStatus {
    /// Every segment ran and the run was scored.
    Completed,
    /// The variant could not be realized here; nothing ran.
    NotAvailable { reason: String },
    /// Something ran and went wrong.
    Failed { error: String },
}

/// The identity of a Brain-served model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainIdentity {
    pub model_id: String,
    pub checkpoint: Option<String>,
    /// The git commit of the Brain server, when it reports one.
    pub commit: Option<String>,
}

/// What produced a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub sven_commit: String,
    /// The working tree had uncommitted changes to tracked files.
    pub sven_dirty: bool,
    pub sven_binary: String,
    pub brain: Option<BrainIdentity>,
    /// SHA-256 of the matrix file text.
    pub matrix_sha256: String,
}

/// A variant as it was actually run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizationRecord {
    pub tool_disclosure: String,
    /// The configuration file the run used, exactly.
    pub config_yaml: String,
    pub context_assembly_requested: String,
    /// The binary reads `context.assembly.mode`. When it does not, the
    /// request had no effect and the variant ran without it.
    pub context_assembly_supported: bool,
}

/// One sven process of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentRecord {
    pub exit_code: i32,
    /// `sven ledger --json` of the segment's trajectory, verbatim.
    pub ledger: serde_json::Value,
    pub trajectory: String,
    pub request_log: String,
    pub stderr: String,
}

/// Input tokens of the whole task, every segment and sub-agent included.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRecord {
    pub total: u64,
    pub requests: u64,
    pub trajectories: u64,
    /// `system`, `tool_schemas`, `user`, `assistant`, `tool_call_arguments`,
    /// `tool_results`, `media`; they sum to `total`.
    pub by_source: BTreeMap<String, u64>,
    pub tool_calls: u64,
    pub duplicate_tool_calls: u64,
    /// Expansions of demoted context: calls that recovered what had been
    /// hidden.
    pub recovery_expansions: u64,
    pub recovery_tokens: u64,
    /// Estimated tokens context assembly saved, summed over requests, by the
    /// reason the results were demoted (`age`, `superseded`, `duplicate`,
    /// `passing_test`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context_saved_by_reason: BTreeMap<String, u64>,
}

impl TokenRecord {
    /// The tokens of a whole task from the ledger summary of each segment.
    #[must_use]
    pub fn from_segments(summaries: &[LedgerSummary]) -> Self {
        let mut record = Self::default();
        for s in summaries {
            record.total += s.estimated_total;
            record.requests += s.requests;
            record.trajectories += s.trajectories;
            record.tool_calls += s.tool_calls;
            record.duplicate_tool_calls += s.duplicate_tool_calls;
            record.recovery_expansions += s.expansions;
            record.recovery_tokens += s.expanded_tokens;
            for (reason, tokens) in &s.context_saved_by_reason {
                *record
                    .context_saved_by_reason
                    .entry(reason.clone())
                    .or_default() += tokens;
            }
            let mut add = |source: &str, tokens: u64| {
                *record.by_source.entry(source.to_string()).or_default() += tokens;
            };
            add(
                "system",
                s.system_stable + s.system_runtime.values().sum::<u64>(),
            );
            add("tool_schemas", s.tool_schemas.values().sum());
            add("user", s.user);
            add("assistant", s.assistant);
            add("tool_call_arguments", s.tool_call_arguments.values().sum());
            add("tool_results", s.tool_results.values().sum());
            add("media", s.media);
        }
        record
    }
}

/// The ledger summaries that make up a task's tokens, given the summary of
/// each process's saved trajectory and how many requests each process made
/// (read from its request log).
///
/// A process that loads the trajectory of the one before saves a trajectory
/// that holds both, so its summary already includes the earlier requests and
/// summing every segment would count them again. Each later summary is either
/// cumulative (its requests are its own plus the previous summary's) and the
/// last one is the whole task, or disjoint (its requests are its own) and
/// they are summed. Anything else is an error: a guess would silently change
/// the tokens of the task.
///
/// # Errors
///
/// A segment whose requests fit neither reading.
pub fn task_ledger(
    summaries: &[LedgerSummary],
    own_requests: &[u64],
) -> Result<Vec<LedgerSummary>, String> {
    let cumulative = (1..summaries.len())
        .all(|i| summaries[i].requests == own_requests[i] + summaries[i - 1].requests);
    let disjoint = (0..summaries.len()).all(|i| summaries[i].requests == own_requests[i]);
    if summaries.len() <= 1 || disjoint {
        return Ok(summaries.to_vec());
    }
    if cumulative && summaries[0].requests == own_requests[0] {
        return Ok(summaries.last().cloned().into_iter().collect());
    }
    let at = (0..summaries.len())
        .find(|&i| {
            summaries[i].requests != own_requests[i]
                && (i == 0 || summaries[i].requests != own_requests[i] + summaries[i - 1].requests)
        })
        .unwrap_or(0);
    Err(format!(
        "segment {at}: its ledger holds {} requests but the process made {}; the ledgers neither carry the earlier segments nor stand alone",
        summaries[at].requests, own_requests[at]
    ))
}

/// What the decision runtime did in a run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Inference calls the session made, cache hits excluded.
    pub inference_calls: u64,
    /// Wall time the session waited on them.
    pub latency_ms: f64,
    pub accepted: u64,
    pub abstained: u64,
    /// Accepted verdicts that changed behaviour.
    pub applied: u64,
    /// Every verdict, as the trajectory recorded it.
    pub traces: Vec<serde_json::Value>,
    /// Requests the benchmark's own provider served, when it served them.
    pub served_requests: u64,
}

/// The record of one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema: String,
    /// Always true for runs this crate makes; a run with a real model would
    /// say false.
    pub scripted: bool,
    pub notice: String,
    pub task_id: String,
    pub suite: Suite,
    pub seed: u64,
    pub variant: VariantRef,
    /// `fallback` for a run whose decision provider was unreachable on
    /// purpose.
    pub probe: Option<String>,
    /// How many times the run was started; a process that hangs is started
    /// once more.
    #[serde(default = "one_attempt")]
    pub attempts: u32,
    pub status: RunStatus,
    pub provenance: Provenance,
    pub realization: Option<RealizationRecord>,
    pub segments: Vec<SegmentRecord>,
    pub tokens: Option<TokenRecord>,
    pub decisions: DecisionRecord,
    pub score: Option<Score>,
}

fn one_attempt() -> u32 {
    1
}

impl RunRecord {
    /// A record with no outcome yet.
    #[must_use]
    pub fn new(
        task_id: &str,
        suite: Suite,
        seed: u64,
        variant: &Variant,
        provenance: Provenance,
        status: RunStatus,
    ) -> Self {
        Self {
            schema: RUN_SCHEMA.to_string(),
            scripted: true,
            notice: SCRIPTED_NOTICE.to_string(),
            task_id: task_id.to_string(),
            suite,
            seed,
            variant: variant.into(),
            probe: None,
            attempts: 1,
            status,
            provenance,
            realization: None,
            segments: Vec::new(),
            tokens: None,
            decisions: DecisionRecord::default(),
            score: None,
        }
    }

    /// Whether the run completed and was scored.
    #[must_use]
    pub fn completed(&self) -> bool {
        self.status == RunStatus::Completed && self.score.is_some()
    }
}
