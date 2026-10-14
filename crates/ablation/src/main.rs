// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! `sven-ablation`: validate the variant matrix, generate and score tasks,
//! run suites under variants, and report.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use sven_ablation::evidence::RequestLog;
use sven_ablation::export::{export, to_jsonl, SplitPlan};
use sven_ablation::loader::load_runs;
use sven_ablation::markdown;
use sven_ablation::matrix::Matrix;
use sven_ablation::mcp_stub;
use sven_ablation::plan::{AssemblySettings, ModelChoice};
use sven_ablation::report;
use sven_ablation::runner::{Probe, Runner, RunnerOptions};
use sven_ablation::score::{score, RunEvidence};
use sven_ablation::suites::generate;
use sven_ablation::task::{read_task, CatalogTool, Suite};

#[derive(Parser)]
#[command(
    name = "sven-ablation",
    about = "Ablation benchmarks for the decision-driven harness"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check a variant matrix without running anything.
    Validate {
        #[arg(long, default_value = "benchmarks/ablation/matrix.yaml")]
        matrix: PathBuf,
    },
    /// Write the task of a suite and seed: its repository and `task.json`.
    Generate {
        #[arg(long)]
        suite: String,
        #[arg(long)]
        seed: u64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Score a finished run of a task. Exits 0 for a pass and 1 for a fail;
    /// the verdict and its evidence labels are printed as JSON.
    Score {
        /// The task directory (with `task.json`).
        #[arg(long)]
        task: PathBuf,
        /// The run directory (with `segment-N/requests.jsonl` and `repo/`).
        #[arg(long)]
        run: PathBuf,
    },
    /// Run suites under variants and write raw records to --out.
    Run(Box<RunArgs>),
    /// Turn the raw records under --runs into report.json and report.md.
    Report {
        #[arg(long)]
        runs: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write the decisions of the runs under --runs as training examples.
    ExportFixtures {
        #[arg(long)]
        runs: PathBuf,
        /// The JSON-lines file to write.
        #[arg(long)]
        out: PathBuf,
        /// Seed of the split; the same seed gives the same split.
        #[arg(long, default_value_t = 0)]
        split_seed: u64,
        #[arg(long, default_value_t = 0.2)]
        validation_fraction: f64,
        #[arg(long, default_value_t = 0.2)]
        test_fraction: f64,
        /// A whole task family that is test only (repeatable).
        #[arg(long = "holdout-suite")]
        holdout_suites: Vec<String>,
    },
    /// Serve a tool catalog as an MCP server on stdio (used by the tasks).
    McpStub {
        #[arg(long)]
        catalog: PathBuf,
    },
}

#[derive(clap::Args)]
struct RunArgs {
    /// Where raw records go. Must be outside the sven repository.
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value = "benchmarks/ablation/matrix.yaml")]
    matrix: PathBuf,
    /// The sven checkout the binary was built from.
    #[arg(long, default_value = ".")]
    sven_repo: PathBuf,
    /// The sven binary to measure (default: the release build, else debug).
    #[arg(long)]
    sven_bin: Option<PathBuf>,
    /// Comma-separated suite names (default: all).
    #[arg(long, value_delimiter = ',')]
    suites: Vec<String>,
    /// Comma-separated variant ids (default: all).
    #[arg(long, value_delimiter = ',')]
    variants: Vec<String>,
    /// How many seeds to run, counting up from --first-seed.
    #[arg(long, default_value_t = 5)]
    seeds: u64,
    #[arg(long, default_value_t = 1)]
    first_seed: u64,
    /// A Brain server, for the variants that use a served model.
    #[arg(long)]
    brain_url: Option<String>,
    #[arg(long, default_value = "auto")]
    stock_model: String,
    #[arg(long)]
    trained_model: Option<String>,
    /// The Brain server's commit, when known; recorded as given.
    #[arg(long)]
    brain_commit: Option<String>,
    /// Also run each decision variant once with its provider unreachable.
    #[arg(long)]
    fallback_probe: bool,
    /// Also run each variant on the tasks whose final question recovers with
    /// a scripted model that fetches what a shortened result left out.
    #[arg(long)]
    recovery: bool,
    /// Estimated tokens of conversation above which context assembly demotes
    /// results (default: the product's).
    #[arg(long)]
    assembly_budget: Option<u32>,
    /// Rounds of tool calls whose results context assembly never demotes
    /// (default: the product's).
    #[arg(long)]
    keep_recent_rounds: Option<u32>,
    /// Longest one sven process may run, in seconds.
    #[arg(long, default_value_t = 300)]
    timeout_secs: u64,
    /// A real model, for end-to-end task success later. Not run by default:
    /// the records of such a run carry no scripted verdict.
    #[arg(long)]
    model: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("sven-ablation: {message}");
            ExitCode::from(2)
        }
    }
}

fn read_matrix(path: &Path) -> Result<(Matrix, String), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let matrix = Matrix::from_yaml(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((matrix, text))
}

fn run(command: Command) -> Result<ExitCode, String> {
    match command {
        Command::Validate { matrix } => {
            let (m, _) = read_matrix(&matrix)?;
            println!("{}: {} variants, valid", matrix.display(), m.variants.len());
            Ok(ExitCode::SUCCESS)
        }
        Command::Generate { suite, seed, out } => {
            let suite =
                Suite::from_name(&suite).ok_or_else(|| format!("unknown suite {suite:?}"))?;
            generate(suite, seed)
                .write(&out)
                .map_err(|e| e.to_string())?;
            println!("{}", out.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::Score { task, run } => score_command(&task, &run),
        Command::Run(args) => {
            let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            runtime.block_on(run_command(*args))
        }
        Command::Report { runs, out } => report_command(&runs, &out),
        Command::ExportFixtures {
            runs,
            out,
            split_seed,
            validation_fraction,
            test_fraction,
            holdout_suites,
        } => {
            let holdout_suites = holdout_suites
                .iter()
                .map(|s| Suite::from_name(s).ok_or_else(|| format!("unknown suite {s:?}")))
                .collect::<Result<Vec<_>, _>>()?;
            let plan = SplitPlan {
                seed: split_seed,
                validation_fraction,
                test_fraction,
                holdout_suites,
            };
            let examples = export(&runs, &plan)?;
            std::fs::write(&out, to_jsonl(&examples)?).map_err(|e| e.to_string())?;
            println!("{} examples to {}", examples.len(), out.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::McpStub { catalog } => {
            let text = std::fs::read_to_string(&catalog).map_err(|e| e.to_string())?;
            let tools: Vec<CatalogTool> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let stdin = std::io::stdin();
            mcp_stub::serve(&tools, stdin.lock(), std::io::stdout()).map_err(|e| e.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn score_command(task_dir: &Path, run_dir: &Path) -> Result<ExitCode, String> {
    let task = read_task(task_dir)?;
    let mut segments = Vec::new();
    for index in 0..task.segments.len() {
        segments.push(RequestLog::read(
            &run_dir.join(format!("segment-{index}/requests.jsonl")),
        )?);
    }
    let evidence = RunEvidence {
        segments,
        repo: Some(run_dir.join("repo")),
    };
    let verdict = score(&task, &evidence).map_err(|e| e.0)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&verdict).map_err(|e| e.to_string())?
    );
    Ok(if verdict.passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn report_command(runs_dir: &Path, out: &Path) -> Result<ExitCode, String> {
    let (matrix, _) = read_matrix(&runs_dir.join("matrix.yaml"))?;
    let runs = load_runs(runs_dir)?;
    if runs.is_empty() {
        return Err(format!("no run records under {}", runs_dir.display()));
    }
    let report = report::build(&matrix, &runs);
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(out.join("report.json"), json + "\n").map_err(|e| e.to_string())?;
    std::fs::write(out.join("report.md"), markdown::render(&report)).map_err(|e| e.to_string())?;
    println!("{}", out.display());
    Ok(ExitCode::SUCCESS)
}

fn default_sven_bin(repo: &Path) -> PathBuf {
    let release = repo.join("target/release/sven");
    if release.exists() {
        release
    } else {
        repo.join("target/debug/sven")
    }
}

async fn run_command(args: RunArgs) -> Result<ExitCode, String> {
    let (matrix, matrix_text) = read_matrix(&args.matrix)?;
    let suites: Vec<Suite> = if args.suites.is_empty() {
        Suite::ALL.to_vec()
    } else {
        args.suites
            .iter()
            .map(|s| Suite::from_name(s).ok_or_else(|| format!("unknown suite {s:?}")))
            .collect::<Result<_, _>>()?
    };
    for id in &args.variants {
        if matrix.variants.iter().all(|v| v.id != *id) {
            return Err(format!("the matrix has no variant {id:?}"));
        }
    }
    let model = args
        .model
        .clone()
        .map_or(ModelChoice::Scripted, ModelChoice::Real);
    let options = RunnerOptions {
        sven_bin: args
            .sven_bin
            .clone()
            .unwrap_or_else(|| default_sven_bin(&args.sven_repo)),
        bench_bin: std::env::current_exe().map_err(|e| e.to_string())?,
        sven_repo: args.sven_repo.clone(),
        out: args.out.clone(),
        timeout: Duration::from_secs(args.timeout_secs),
        brain_url: args.brain_url.clone(),
        stock_model: args.stock_model.clone(),
        trained_model: args.trained_model.clone(),
        brain_commit: args.brain_commit.clone(),
        model,
        assembly: AssemblySettings {
            budget_tokens: args.assembly_budget,
            keep_recent_rounds: args.keep_recent_rounds,
        },
    };
    let runner = Runner::new(options, &matrix_text).await?;
    std::fs::write(args.out.join("matrix.yaml"), &matrix_text).map_err(|e| e.to_string())?;
    if !runner.assembly_supported() {
        eprintln!(
            "note: this sven binary has no context.assembly.mode key; variants ran without it"
        );
    }
    let mut problems = 0;
    for seed in args.first_seed..args.first_seed + args.seeds {
        for suite in &suites {
            let generated = generate(*suite, seed);
            for variant in matrix
                .variants
                .iter()
                .filter(|v| args.variants.is_empty() || args.variants.contains(&v.id))
            {
                let record = runner.run(&generated, variant, None).await;
                eprintln!("{} {} {:?}", generated.task.id, variant.id, record.status);
                problems += usize::from(matches!(
                    record.status,
                    sven_ablation::record::RunStatus::Failed { .. }
                ));
                if args.recovery && generated.task.recovers() {
                    let record = runner.run(&generated, variant, Some(Probe::Recovery)).await;
                    eprintln!(
                        "{} {} recovery {:?}",
                        generated.task.id, variant.id, record.status
                    );
                    problems += usize::from(matches!(
                        record.status,
                        sven_ablation::record::RunStatus::Failed { .. }
                    ));
                }
                let probe = args.fallback_probe
                    && seed == args.first_seed
                    && variant.decisions.is_injected()
                    && matches!(suite, Suite::ToolSelection | Suite::RelevantFile);
                if probe {
                    let record = runner.run(&generated, variant, Some(Probe::Fallback)).await;
                    eprintln!(
                        "{} {} fallback {:?}",
                        generated.task.id, variant.id, record.status
                    );
                }
            }
        }
    }
    println!("{}", args.out.display());
    Ok(if problems == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
