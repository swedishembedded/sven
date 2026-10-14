// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The task format: what a generator produces and a scorer reads.
//!
//! A task is the same for every variant: one repository, one scripted
//! conversation, one tool catalog, one set of labelled evidence. Nothing in
//! it depends on the variant under test.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::providers::OracleLabel;

/// The only task format this build reads and writes.
pub const TASK_SCHEMA: &str = "sven.ablation.task/1";

/// The suites, one per question the benchmark can answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Suite {
    RelevantFile,
    NoisyOutput,
    ToolSelection,
    RestartSurvival,
    Delegation,
    Security,
}

impl Suite {
    pub const ALL: [Suite; 6] = [
        Self::RelevantFile,
        Self::NoisyOutput,
        Self::ToolSelection,
        Self::RestartSurvival,
        Self::Delegation,
        Self::Security,
    ];

    /// The name used in files, flags and reports.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::RelevantFile => "relevant_file",
            Self::NoisyOutput => "noisy_output",
            Self::ToolSelection => "tool_selection",
            Self::RestartSurvival => "restart_survival",
            Self::Delegation => "delegation",
            Self::Security => "security",
        }
    }

    /// The suite called `name`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// One tool call the scripted model makes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptedCall {
    pub id: String,
    pub tool: String,
    pub args: serde_json::Value,
}

/// One user instruction and the scripted model's answer to it: a round of
/// parallel tool calls, then a text reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    /// A string unique to this turn that the user text contains; the scripted
    /// model recognises its turn by it.
    pub marker: String,
    pub user: String,
    pub calls: Vec<ScriptedCall>,
    pub reply: String,
}

/// The turns run by one sven process. A cold restart is a new segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub turns: Vec<Turn>,
    /// Instructions the sub-agents of this segment receive and the scripted
    /// model's answers to them. They are not user turns: the parent sends
    /// each as the prompt of a delegated task.
    #[serde(default)]
    pub children: Vec<Turn>,
}

/// Whether a piece of evidence is needed to do the task or only looks like it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRole {
    /// Losing it makes the task unanswerable.
    Critical,
    /// Matches the same searches but does not matter.
    Distractor,
}

/// A labelled piece of evidence: `marker` is a unique string that occurs
/// exactly where the evidence does, so its presence in what the model was
/// sent is checkable by substring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub marker: String,
    pub role: EvidenceRole,
    /// `file_line`, `log_line`, `constraint`, `finding`, `rule`, `result`.
    pub kind: String,
    /// The file it is in, or `user` / `tool` / `system`.
    pub source: String,
}

/// How a search query is phrased.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryStyle {
    /// Uses the words of the tool's name.
    Exact,
    /// Says the same thing in other words.
    Paraphrase,
    /// Names the domain only indirectly.
    Indirect,
}

/// A query a model would search the tool catalog with and the one tool that
/// answers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchProbe {
    pub query: String,
    pub style: QueryStyle,
    /// The name of the tool, as the model is shown it.
    pub target: String,
}

/// One tool the stub MCP server offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogTool {
    pub name: String,
    pub description: String,
}

/// A secret that must never reach the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Canary {
    pub marker: String,
    pub path: String,
}

/// What must hold after a security task: any breach is a hard failure.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SecuritySpec {
    /// Secrets that must not appear in any request.
    pub canaries: Vec<Canary>,
    /// Files whose content must be unchanged (path, SHA-256 hex).
    pub protected_files: Vec<(String, String)>,
    /// Files that a denied action would have removed; they must still exist.
    pub must_still_exist: Vec<String>,
    /// Markers of allowed actions' output that must reach the model, so a
    /// guard that blocks everything is not mistaken for a safe one.
    pub must_reach_model: Vec<String>,
}

/// A task: the same for every variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub schema: String,
    pub id: String,
    pub suite: Suite,
    pub seed: u64,
    /// What this suite measures, and what it does not.
    pub measures: String,
    /// `agent`, `research` or `plan`.
    pub mode: String,
    /// Configuration every variant runs with, merged under the variant's.
    pub base_config_yaml: String,
    pub segments: Vec<Segment>,
    pub evidence: Vec<Evidence>,
    pub probes: Vec<SearchProbe>,
    pub catalog: Vec<CatalogTool>,
    pub security: Option<SecuritySpec>,
    /// The right answer of every decision the task provokes.
    pub oracle_labels: Vec<OracleLabel>,
}

impl Task {
    /// A task of `suite` and `seed` with nothing in it yet.
    #[must_use]
    pub fn new(suite: Suite, seed: u64, measures: &str) -> Self {
        Self {
            schema: TASK_SCHEMA.to_string(),
            id: format!("{}-s{seed}", suite.name()),
            suite,
            seed,
            measures: measures.to_string(),
            mode: "agent".into(),
            base_config_yaml: String::new(),
            segments: Vec::new(),
            evidence: Vec::new(),
            probes: Vec::new(),
            catalog: Vec::new(),
            security: None,
            oracle_labels: Vec::new(),
        }
    }

    pub fn critical(&self) -> impl Iterator<Item = &Evidence> {
        self.evidence
            .iter()
            .filter(|e| e.role == EvidenceRole::Critical)
    }

    pub fn distractors(&self) -> impl Iterator<Item = &Evidence> {
        self.evidence
            .iter()
            .filter(|e| e.role == EvidenceRole::Distractor)
    }
}

/// A generated task and the files of its repository.
#[derive(Debug, Clone, PartialEq)]
pub struct Generated {
    pub task: Task,
    /// Repository files by relative path.
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Generated {
    /// Writes `task.json` and the repository under `dir/repo`.
    ///
    /// # Errors
    ///
    /// The first I/O or serialization failure.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        for (relative, bytes) in &self.files {
            let path = dir.join("repo").join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, bytes)?;
        }
        std::fs::create_dir_all(dir.join("repo"))?;
        let json = serde_json::to_string_pretty(&self.task).map_err(std::io::Error::other)?;
        std::fs::write(dir.join("task.json"), json + "\n")
    }
}

/// Reads `task.json` from a task directory.
///
/// # Errors
///
/// A missing file, invalid JSON or another schema version.
pub fn read_task(dir: &Path) -> Result<Task, String> {
    let path = dir.join("task.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let task: Task = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if task.schema != TASK_SCHEMA {
        return Err(format!(
            "{}: task schema {:?} is not {TASK_SCHEMA:?}",
            path.display(),
            task.schema
        ));
    }
    Ok(task)
}
