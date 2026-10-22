// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven init` and `sven config get|set`: the configuration files written from
//! the command line, one key at a time, each checked as sven reads it.

use std::path::{Path, PathBuf};

use sven_config::scope::{self, Scope};
use sven_config::ConfigDocument;
use sven_frontend::Settings;

use crate::cli::ConfigCommands;
use crate::run::secret::read_secret;

/// The model `sven init` writes: an OpenRouter GLM model, cheap and able to
/// call tools.
const INIT_PROVIDER: &str = "openrouter";
const INIT_MODEL: &str = "z-ai/glm-5.3-flash";

/// `sven init`: a starting `.sven.yaml` here, never over an existing file.
pub(crate) fn run_init() -> anyhow::Result<()> {
    let here = std::env::current_dir()?;
    if let Some(existing) = scope::directory_file(&here) {
        anyhow::bail!(
            "{} already exists; change it with `sven config set <key> <value>`",
            existing.display()
        );
    }
    let path = here.join(scope::NEW_FILE_NAME);
    let document = [("provider", INIT_PROVIDER), ("name", INIT_MODEL)]
        .into_iter()
        .try_fold(serde_yaml::Value::Null, |doc, (key, value)| {
            scope::with_value_at(doc, &["model".into(), key.into()], value.into())
        })
        .map_err(anyhow::Error::msg)?;
    scope::write_file(&path, &document)?;
    println!(
        "wrote {} ({INIT_PROVIDER}/{INIT_MODEL}, no key)",
        path.display()
    );
    println!("store the key once with `sven auth set {INIT_PROVIDER}`; `sven config set <key> <value>` changes the file");
    Ok(())
}

pub(crate) fn run_config_command(
    command: &ConfigCommands,
    extra: Option<&Path>,
) -> anyhow::Result<()> {
    let here = std::env::current_dir()?;
    match command {
        ConfigCommands::Get { key, scope } => get(&here, key, *scope, extra),
        ConfigCommands::Set {
            key,
            value,
            stdin,
            scope,
        } => {
            let text = match value {
                Some(value) => value.clone(),
                None if *stdin => read_secret(&format!("{key}: "))?
                    .trim_end_matches(['\r', '\n'])
                    .to_string(),
                None => anyhow::bail!("give a value, or --stdin"),
            };
            set(&here, key, &text, *scope)
        }
    }
}

/// The file `scope` names from `here`, or why it names none.
fn file_of(scope: Scope, here: &Path) -> anyhow::Result<PathBuf> {
    scope::scope_file(scope, here).ok_or_else(|| match scope {
        Scope::Workspace => anyhow::anyhow!(
            "no workspace configuration: no directory above {} holds a .sven.yaml; make one there, or choose --scope project or user",
            here.display()
        ),
        _ => anyhow::anyhow!("no {scope} configuration file can be named here"),
    })
}

/// Refuse a key sven does not know before reading or writing anything.
fn known_key(path: &[String], text: &str) -> anyhow::Result<()> {
    let probe = scope::with_value_at(serde_yaml::Value::Null, path, serde_yaml::Value::Null)
        .map_err(anyhow::Error::msg)?;
    let unknown = ConfigDocument::from_value(probe).unknown_keys(&Settings::schema());
    anyhow::ensure!(unknown.is_empty(), "{text}: {}", unknown.join("; "));
    Ok(())
}

fn set(here: &Path, key: &str, text: &str, scope: Scope) -> anyhow::Result<()> {
    let path = scope::parse_path(key).map_err(anyhow::Error::msg)?;
    known_key(&path, key)?;
    let value: serde_yaml::Value = serde_yaml::from_str(text).unwrap_or_else(|_| text.into());
    let file = file_of(scope, here)?;
    let document =
        scope::with_value_at(scope::read_file(&file)?, &path, value).map_err(anyhow::Error::msg)?;
    Settings::validate(&ConfigDocument::from_value(document.clone()))
        .map_err(|e| anyhow::anyhow!("{key}: {e}; {} is unchanged", file.display()))?;
    scope::write_file(&file, &document)?;
    eprintln!("set {key} in {}", file.display());
    Ok(())
}

fn get(here: &Path, key: &str, scope: Option<Scope>, extra: Option<&Path>) -> anyhow::Result<()> {
    let path = scope::parse_path(key).map_err(anyhow::Error::msg)?;
    known_key(&path, key)?;
    let found = match scope {
        Some(scope) => {
            let file = file_of(scope, here)?;
            let value = scope::value_at(&scope::read_file(&file)?, &path).cloned();
            value.ok_or_else(|| anyhow::anyhow!("{key} is not set in {}", file.display()))?
        }
        None => {
            // What is in force: as the merged files write it, else the
            // default (a value read back through its type would lose how it
            // was written, `0.3` as an f32 included).
            let written = ConfigDocument::load(extra)?;
            let settings = serde_yaml::to_value(Settings::load(extra)?)?;
            scope::value_at(written.root(), &path)
                .or_else(|| scope::value_at(&settings, &path))
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!("{key} has no value: it is set nowhere and has no default")
                })?
        }
    };
    print!("{}", shown(&found)?);
    Ok(())
}

/// A value as a person reads it back: text bare, anything else as YAML.
fn shown(value: &serde_yaml::Value) -> anyhow::Result<String> {
    Ok(match value {
        serde_yaml::Value::String(s) => format!("{s}\n"),
        other => serde_yaml::to_string(other)?,
    })
}
