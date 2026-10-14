// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Runs one task under one variant: the repository, the configuration, the
//! decision provider, every sven process, the ledger, the score, and a raw
//! record of all of it in the run directory.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use sven_decision::brain::{BrainProvider, BrainSettings, MinimumQuality};
use sven_decision::DecisionProvider;
use sven_vocab::LedgerSummary;
use tokio::process::Command;

use crate::evidence::RequestLog;
use crate::matrix::{Decisions, Endpoints, Variant};
use crate::mockscript::{render_conversation, render_mock};
use crate::plan::{compose_config, invocation, Invocation, ModelChoice, RunLayout};
use crate::provider_server::ProviderServer;
use crate::providers::{HeuristicProvider, OracleProvider, RandomProvider};
use crate::record::{
    BrainIdentity, Provenance, RealizationRecord, RunRecord, RunStatus, SegmentRecord, TokenRecord,
};
use crate::score::{score, RunEvidence};
use crate::task::{Generated, Task};
use crate::trajectory::decision_record;

/// Exit status of a run that finished but had a tool call fail. The scripted
/// attempts of the security suite are refused on purpose, so it is an outcome
/// of a run, not a failure of it.
const EXIT_TOOL_ERRORS: i32 = 3;

/// How a run differs from an ordinary one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// The decision provider is unreachable on purpose: the run must still
    /// complete.
    Fallback,
}

impl Probe {
    fn name(self) -> &'static str {
        match self {
            Self::Fallback => "fallback",
        }
    }
}

/// What the invocation of the benchmark was told.
#[derive(Debug, Clone)]
pub struct RunnerOptions {
    pub sven_bin: PathBuf,
    /// This executable, which serves the stub MCP catalog.
    pub bench_bin: PathBuf,
    /// The checkout the sven binary was built from, for provenance.
    pub sven_repo: PathBuf,
    pub out: PathBuf,
    /// The longest one sven process may run.
    pub timeout: Duration,
    pub brain_url: Option<String>,
    pub stock_model: String,
    pub trained_model: Option<String>,
    pub brain_commit: Option<String>,
    pub model: ModelChoice,
}

/// A run that cannot go on, and why.
enum Stop {
    Unavailable(String),
    Failed(String),
}

impl From<String> for Stop {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

fn failed(context: &str, error: impl std::fmt::Display) -> Stop {
    Stop::Failed(format!("{context}: {error}"))
}

/// Runs tasks under the variants of a matrix.
pub struct Runner {
    options: RunnerOptions,
    matrix_sha256: String,
    sven_commit: String,
    sven_dirty: bool,
    assembly_supported: bool,
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Runner {
    /// A runner for `options`, over the matrix whose file text is
    /// `matrix_text`.
    ///
    /// # Errors
    ///
    /// The sven binary is missing, or `out` is inside the repository it
    /// measures.
    pub async fn new(mut options: RunnerOptions, matrix_text: &str) -> Result<Self, String> {
        if !options.sven_bin.exists() {
            return Err(format!(
                "the sven binary {} does not exist (run `make build`)",
                options.sven_bin.display()
            ));
        }
        let repo = std::fs::canonicalize(&options.sven_repo).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&options.out).map_err(|e| e.to_string())?;
        let out = std::fs::canonicalize(&options.out).map_err(|e| e.to_string())?;
        if out.starts_with(&repo) {
            return Err(format!(
                "--out must be outside the repository: {}",
                out.display()
            ));
        }
        // Processes run in the repository of each task, so every path must be
        // absolute.
        options.sven_bin = std::fs::canonicalize(&options.sven_bin).map_err(|e| e.to_string())?;
        options.bench_bin = std::fs::canonicalize(&options.bench_bin).map_err(|e| e.to_string())?;
        options.out = out;
        let assembly_supported = probe_assembly(&options).await;
        Ok(Self {
            matrix_sha256: hex::encode(Sha256::digest(matrix_text.as_bytes())),
            sven_commit: git(&repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into()),
            sven_dirty: git(&repo, &["status", "--porcelain", "--untracked-files=no"])
                .is_none_or(|s| !s.is_empty()),
            assembly_supported,
            options,
        })
    }

    /// Whether the binary reads `context.assembly.mode`.
    #[must_use]
    pub fn assembly_supported(&self) -> bool {
        self.assembly_supported
    }

    fn provenance(&self) -> Provenance {
        Provenance {
            sven_commit: self.sven_commit.clone(),
            sven_dirty: self.sven_dirty,
            sven_binary: self.options.sven_bin.display().to_string(),
            brain: None,
            matrix_sha256: self.matrix_sha256.clone(),
        }
    }

    /// Runs `generated` under `variant` and writes the record next to the
    /// run's files. Never panics and never skips: an unavailable variant and
    /// a failed run are records too.
    pub async fn run(
        &self,
        generated: &Generated,
        variant: &Variant,
        probe: Option<Probe>,
    ) -> RunRecord {
        let task = &generated.task;
        let mut record = RunRecord::new(
            &task.id,
            task.suite,
            task.seed,
            variant,
            self.provenance(),
            RunStatus::Completed,
        );
        record.probe = probe.map(|p| p.name().to_string());
        record.scripted = self.options.model == ModelChoice::Scripted;
        let label = match probe {
            Some(p) => format!("{}-{}", variant.id, p.name()),
            None => variant.id.clone(),
        };
        let layout = RunLayout {
            dir: self.options.out.join("runs").join(&task.id).join(label),
        };
        if let Err(stop) = self
            .execute(generated, variant, probe, &layout, &mut record)
            .await
        {
            record.status = match stop {
                Stop::Unavailable(reason) => RunStatus::NotAvailable { reason },
                Stop::Failed(error) => RunStatus::Failed { error },
            };
        }
        match serde_json::to_string_pretty(&record) {
            Ok(json) => {
                if let Err(e) = std::fs::create_dir_all(&layout.dir)
                    .and_then(|()| std::fs::write(layout.dir.join("record.json"), json + "\n"))
                {
                    record.status = RunStatus::Failed {
                        error: format!("the record could not be written: {e}"),
                    };
                }
            }
            Err(e) => {
                record.status = RunStatus::Failed {
                    error: format!("the record could not be serialized: {e}"),
                }
            }
        }
        record
    }

    async fn execute(
        &self,
        generated: &Generated,
        variant: &Variant,
        probe: Option<Probe>,
        layout: &RunLayout,
        record: &mut RunRecord,
    ) -> Result<(), Stop> {
        let task = &generated.task;
        if layout.dir.exists() {
            std::fs::remove_dir_all(&layout.dir).map_err(|e| failed("clearing the run", e))?;
        }
        generated
            .write(&layout.dir)
            .map_err(|e| failed("writing the task", e))?;
        std::fs::create_dir_all(layout.home()).map_err(|e| failed("creating the home", e))?;
        let catalog = serde_json::to_string(&task.catalog).map_err(|e| failed("catalog", e))?;
        std::fs::write(layout.catalog(), catalog).map_err(|e| failed("catalog", e))?;

        let served = self.serve_decisions(variant, task, probe, record).await?;
        let endpoints = Endpoints {
            local_provider_url: served.url.clone(),
            brain_url: self.options.brain_url.clone(),
            stock_model: self.options.stock_model.clone(),
            trained_model: self.options.trained_model.clone(),
        };
        let mut realization = variant
            .realize(&endpoints)
            .map_err(|u| Stop::Unavailable(u.to_string()))?;
        if probe == Some(Probe::Fallback) {
            realization.config_yaml = realization
                .config_yaml
                .replace("mode: required", "mode: auto");
        }
        record.realization = Some(RealizationRecord {
            tool_disclosure: realization.tool_disclosure.flag_value().to_string(),
            config_yaml: realization.config_yaml.clone(),
            context_assembly_requested: variant.context_assembly.clone(),
            context_assembly_supported: self.assembly_supported,
        });
        let config = compose_config(
            task,
            &realization,
            &self.options.bench_bin,
            &layout.catalog(),
        )?;
        std::fs::write(layout.config(), config).map_err(|e| failed("config", e))?;

        let mut summaries: Vec<LedgerSummary> = Vec::new();
        let mut logs: Vec<RequestLog> = Vec::new();
        let mut last_trajectory = serde_json::Value::Null;
        for (index, segment) in task.segments.iter().enumerate() {
            let dir = layout.segment(index);
            std::fs::create_dir_all(&dir).map_err(|e| failed("segment", e))?;
            let log_path = dir.join("requests.jsonl");
            std::fs::write(
                dir.join("conversation.md"),
                render_conversation(task, index),
            )
            .map_err(|e| failed("conversation", e))?;
            std::fs::write(dir.join("mock.yaml"), render_mock(segment, &log_path))
                .map_err(|e| failed("mock script", e))?;
            let inv = invocation(task, &realization, layout, &self.options.model, index);
            let exit_code = self.spawn(&inv, &dir).await?;
            let trajectory_path = dir.join("trajectory.json");
            let ledger = self.ledger_json(&trajectory_path).await?;
            let summary: LedgerSummary = serde_json::from_value(ledger.clone())
                .map_err(|e| failed("reading the ledger summary", e))?;
            last_trajectory = std::fs::read_to_string(&trajectory_path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(serde_json::Value::Null);
            record.segments.push(SegmentRecord {
                exit_code,
                ledger,
                trajectory: relative(layout, &trajectory_path),
                request_log: relative(layout, &log_path),
                stderr: relative(layout, &dir.join("stderr.txt")),
            });
            summaries.push(summary);
            if self.options.model == ModelChoice::Scripted {
                logs.push(RequestLog::read(&log_path)?);
            }
            if exit_code != 0 && exit_code != EXIT_TOOL_ERRORS {
                return Err(Stop::Failed(format!(
                    "segment {index} exited with status {exit_code}; see {}",
                    dir.join("stderr.txt").display()
                )));
            }
        }
        record.tokens = Some(TokenRecord::from_segments(&summaries));
        record.decisions = decision_record(&last_trajectory);
        record.decisions.served_requests =
            served.server.as_ref().map_or(0, |s| s.stats().decisions());
        if self.options.model == ModelChoice::Scripted {
            let evidence = RunEvidence {
                segments: logs,
                repo: Some(layout.repo()),
            };
            record.score = Some(score(task, &evidence).map_err(|e| Stop::Failed(e.0))?);
        }
        Ok(())
    }

    /// Starts the benchmark's own provider for a variant that has one, or
    /// points the run at a closed port for the fallback probe.
    async fn serve_decisions(
        &self,
        variant: &Variant,
        task: &Task,
        probe: Option<Probe>,
        record: &mut RunRecord,
    ) -> Result<Served, Stop> {
        let none = Served {
            url: String::new(),
            server: None,
        };
        if let Decisions::Brain { .. } = variant.decisions {
            self.check_brain(variant, record).await?;
            return Ok(none);
        }
        let Some(model_id) = variant.decisions.injected_model_id() else {
            return Ok(none);
        };
        if probe == Some(Probe::Fallback) {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")
                .map_err(|e| failed("reserving a closed port", e))?;
            let url = format!(
                "http://{}",
                listener.local_addr().map_err(|e| failed("port", e))?
            );
            drop(listener);
            return Ok(Served { url, server: None });
        }
        let provider: Arc<dyn DecisionProvider> = match variant.decisions {
            Decisions::Heuristic => Arc::new(HeuristicProvider::new()),
            Decisions::Random => Arc::new(RandomProvider::new(task.seed)),
            _ => Arc::new(OracleProvider::new(task.oracle_labels.clone())),
        };
        let server = ProviderServer::start(provider, model_id)
            .await
            .map_err(|e| failed("starting the decision provider", e))?;
        Ok(Served {
            url: server.url(),
            server: Some(server),
        })
    }

    /// A Brain variant runs only when its server answers and reports the
    /// model; otherwise it is recorded as not available.
    async fn check_brain(&self, variant: &Variant, record: &mut RunRecord) -> Result<(), Stop> {
        let Some(url) = self.options.brain_url.as_deref() else {
            return Ok(());
        };
        let wanted = match variant.decisions {
            Decisions::Brain {
                model: crate::matrix::BrainModel::Trained,
            } => self.options.trained_model.clone().unwrap_or_default(),
            _ => self.options.stock_model.clone(),
        };
        let mut settings = BrainSettings::new(url);
        settings.model = wanted;
        settings.minimum_quality = MinimumQuality::Any;
        let provider = BrainProvider::new(settings).map_err(Stop::Failed)?;
        match provider.readiness(Duration::from_secs(5)).await {
            Ok(info) => {
                record.provenance.brain = Some(BrainIdentity {
                    model_id: info.id,
                    checkpoint: info.checkpoint,
                    commit: self.options.brain_commit.clone(),
                });
                Ok(())
            }
            Err(e) => Err(Stop::Unavailable(format!(
                "not available: the Brain server at {url} cannot serve variant {}: {e}",
                variant.id
            ))),
        }
    }

    async fn spawn(&self, inv: &Invocation, dir: &Path) -> Result<i32, Stop> {
        let stdout =
            std::fs::File::create(dir.join("stdout.txt")).map_err(|e| failed("stdout", e))?;
        let stderr =
            std::fs::File::create(dir.join("stderr.txt")).map_err(|e| failed("stderr", e))?;
        let mut command = Command::new(&self.options.sven_bin);
        command
            .args(&inv.args)
            .envs(inv.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&inv.cwd)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(true);
        let child = command.spawn().map_err(|e| failed("starting sven", e))?;
        match tokio::time::timeout(self.options.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => Ok(output.status.code().unwrap_or(-1)),
            Ok(Err(e)) => Err(failed("waiting for sven", e)),
            Err(_) => Err(Stop::Failed(format!(
                "sven did not finish within {} s",
                self.options.timeout.as_secs()
            ))),
        }
    }

    async fn ledger_json(&self, trajectory: &Path) -> Result<serde_json::Value, Stop> {
        let output = Command::new(&self.options.sven_bin)
            .arg("ledger")
            .arg(trajectory)
            .arg("--json")
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| failed("running `sven ledger`", e))?;
        if !output.status.success() {
            return Err(Stop::Failed(format!(
                "`sven ledger {}` failed: {}",
                trajectory.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|e| failed("reading `sven ledger --json`", e))
    }
}

struct Served {
    url: String,
    server: Option<ProviderServer>,
}

fn relative(layout: &RunLayout, path: &Path) -> String {
    path.strip_prefix(&layout.dir)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Whether the binary reads `context.assembly.mode`: a configuration that
/// sets it must show the section in `show-config`.
async fn probe_assembly(options: &RunnerOptions) -> bool {
    let dir = options.out.join(".probe");
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let config = dir.join("config.yaml");
    if std::fs::write(&config, "context:\n  assembly:\n    mode: off\n").is_err() {
        return false;
    }
    let output = Command::new(&options.sven_bin)
        .arg("--config")
        .arg(&config)
        .arg("show-config")
        .env("HOME", &dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .stdin(Stdio::null())
        .output()
        .await;
    let _ = std::fs::remove_dir_all(&dir);
    output.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("assembly"))
}
