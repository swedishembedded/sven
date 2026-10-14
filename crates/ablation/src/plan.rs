// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The sven processes that realize one run, as plain data.
//!
//! Building the commands is separate from running them so that what a
//! variant does to the command line can be read and tested without a binary.

use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};

use crate::matrix::Realization;
use crate::task::Task;

/// The name the stub MCP server is configured under.
pub const MCP_SERVER: &str = "catalog";

/// Where a run keeps its files.
#[derive(Debug, Clone)]
pub struct RunLayout {
    pub dir: PathBuf,
}

impl RunLayout {
    #[must_use]
    pub fn repo(&self) -> PathBuf {
        self.dir.join("repo")
    }
    #[must_use]
    pub fn home(&self) -> PathBuf {
        self.dir.join("home")
    }
    #[must_use]
    pub fn config(&self) -> PathBuf {
        self.dir.join("config.yaml")
    }
    #[must_use]
    pub fn catalog(&self) -> PathBuf {
        self.dir.join("catalog.json")
    }
    #[must_use]
    pub fn segment(&self, index: usize) -> PathBuf {
        self.dir.join(format!("segment-{index}"))
    }
}

/// One sven process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

/// How the processes of a run are told which model to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChoice {
    /// The scripted model of the task.
    Scripted,
    /// A real model, named explicitly. Never the default.
    Real(String),
}

/// Merges `from` into `into`; mappings merge key by key, anything else is
/// replaced.
pub fn merge(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Mapping(a), Value::Mapping(b)) => {
            for (key, value) in b {
                match a.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        a.insert(key, value);
                    }
                }
            }
        }
        (slot, value) => *slot = value,
    }
}

/// What context assembly may spend and keep, when a run is not to use the
/// product's defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AssemblySettings {
    pub budget_tokens: Option<u32>,
    pub keep_recent_rounds: Option<u32>,
}

/// `config_yaml` with `settings` written under `context.assembly`.
///
/// # Errors
///
/// A configuration that is not YAML.
pub fn with_assembly_settings(
    config_yaml: &str,
    settings: &AssemblySettings,
) -> Result<String, String> {
    if *settings == AssemblySettings::default() {
        return Ok(config_yaml.to_string());
    }
    let mut config: Value = serde_yaml::from_str(config_yaml).map_err(|e| e.to_string())?;
    let mut assembly = serde_json::Map::new();
    if let Some(n) = settings.budget_tokens {
        assembly.insert("budget_tokens".into(), n.into());
    }
    if let Some(n) = settings.keep_recent_rounds {
        assembly.insert("keep_recent_rounds".into(), n.into());
    }
    let fragment = serde_yaml::to_value(serde_json::json!({"context": {"assembly": assembly}}))
        .map_err(|e| e.to_string())?;
    merge(&mut config, fragment);
    serde_yaml::to_string(&config).map_err(|e| e.to_string())
}

/// The assembly settings a configuration runs with, as a report states them;
/// `None` when it does not assemble context.
#[must_use]
pub fn assembly_description(config_yaml: &str) -> Option<String> {
    let config: Value = serde_yaml::from_str(config_yaml).ok()?;
    let assembly = config.get("context")?.get("assembly")?;
    if assembly.get("mode")?.as_str()? == "off" {
        return None;
    }
    let setting = |key: &str| {
        assembly
            .get(key)
            .and_then(Value::as_u64)
            .map_or_else(|| "default".to_string(), |n| n.to_string())
    };
    Some(format!(
        "budget_tokens {}, keep_recent_rounds {}",
        setting("budget_tokens"),
        setting("keep_recent_rounds")
    ))
}

/// The configuration file of a run: the task's base configuration, the
/// variant's, and the stub MCP server when the task has a catalog.
///
/// # Errors
///
/// A configuration fragment that is not YAML.
pub fn compose_config(
    task: &Task,
    realization: &Realization,
    bench_bin: &Path,
    catalog: &Path,
) -> Result<String, String> {
    let mut config = Value::Mapping(Mapping::new());
    for (name, text) in [
        (
            "the task's base configuration",
            task.base_config_yaml.as_str(),
        ),
        (
            "the variant's configuration",
            realization.config_yaml.as_str(),
        ),
    ] {
        if text.trim().is_empty() {
            continue;
        }
        let fragment: Value = serde_yaml::from_str(text).map_err(|e| format!("{name}: {e}"))?;
        merge(&mut config, fragment);
    }
    if !task.catalog.is_empty() {
        let server = serde_yaml::to_value(serde_json::json!({
            "mcp_servers": {
                MCP_SERVER: {
                    "transport": {
                        "type": "stdio",
                        "command": bench_bin.display().to_string(),
                        "args": ["mcp-stub", "--catalog", catalog.display().to_string()],
                    },
                    "timeout_secs": 30,
                }
            }
        }))
        .map_err(|e| e.to_string())?;
        merge(&mut config, server);
    }
    serde_yaml::to_string(&config).map_err(|e| e.to_string())
}

/// The process that runs segment `index` of `task`.
#[must_use]
pub fn invocation(
    task: &Task,
    realization: &Realization,
    layout: &RunLayout,
    model: &ModelChoice,
    index: usize,
) -> Invocation {
    let segment = layout.segment(index);
    let mut args: Vec<String> = vec![
        "--headless".into(),
        "--model".into(),
        match model {
            ModelChoice::Scripted => "mock".into(),
            ModelChoice::Real(name) => name.clone(),
        },
        "--token-ledger".into(),
        "--config".into(),
        layout.config().display().to_string(),
        "--tool-disclosure".into(),
        realization.tool_disclosure.flag_value().into(),
        "--mode".into(),
        task.mode.clone(),
        "--approval".into(),
        "auto".into(),
        "--output-trace".into(),
        segment.join("trajectory.json").display().to_string(),
    ];
    if index > 0 {
        args.push("--load-trace".into());
        args.push(
            layout
                .segment(index - 1)
                .join("trajectory.json")
                .display()
                .to_string(),
        );
    }
    args.push("--file".into());
    args.push(segment.join("conversation.md").display().to_string());

    let home = layout.home();
    let mut env: Vec<(String, String)> = vec![
        ("HOME".into(), home.display().to_string()),
        (
            "XDG_CONFIG_HOME".into(),
            home.join(".config").display().to_string(),
        ),
        (
            "XDG_DATA_HOME".into(),
            home.join(".local/share").display().to_string(),
        ),
        ("OPENAI_API_KEY".into(), "bench-dummy-key".into()),
        ("ANTHROPIC_API_KEY".into(), "bench-dummy-key".into()),
    ];
    if *model == ModelChoice::Scripted {
        env.push((
            "SVEN_MOCK_RESPONSES".into(),
            segment.join("mock.yaml").display().to_string(),
        ));
    }
    Invocation {
        args,
        env,
        cwd: layout.repo(),
    }
}
