// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for giving agents new tools without
// hand-editing configuration for its clients. If your team needs expertise in
// Model Context Protocol integration then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Adding and removing a server in a configuration file (`sven mcp add`,
//! `/mcp add`), so neither frontend edits YAML of its own.

use std::path::PathBuf;

use sven_config::scope::{self, Scope};

/// The `mcp_servers.<name>` entry for a server reached at `target`: a URL is
/// a Streamable HTTP server, anything else the command (and arguments) of a
/// stdio server.
///
/// # Errors
///
/// The options do not fit the transport, or the entry is no valid server.
pub fn server_entry(
    target: &[String],
    headers: &[String],
    env: &[String],
    oauth: bool,
) -> anyhow::Result<serde_yaml::Value> {
    let pairs = |items: &[String], what: &str| -> anyhow::Result<serde_yaml::Mapping> {
        items
            .iter()
            .map(|item| {
                let (k, v) = item
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("{what} `{item}` is not NAME=VALUE"))?;
                Ok((k.trim().into(), v.into()))
            })
            .collect()
    };
    let mut entry = serde_yaml::Mapping::new();
    let first = &target[0];
    let transport: serde_yaml::Value =
        if first.starts_with("https://") || first.starts_with("http://") {
            anyhow::ensure!(target.len() == 1, "an HTTP server takes its URL alone");
            anyhow::ensure!(
                env.is_empty(),
                "--env is for a stdio server; an HTTP one takes --header"
            );
            let mut t = serde_yaml::Mapping::new();
            t.insert("type".into(), "http".into());
            t.insert("url".into(), first.as_str().into());
            if !headers.is_empty() {
                t.insert("headers".into(), pairs(headers, "header")?.into());
            }
            t.into()
        } else {
            anyhow::ensure!(headers.is_empty(), "--header is for an HTTP server");
            anyhow::ensure!(!oauth, "--oauth is for an HTTP server");
            let mut t = serde_yaml::Mapping::new();
            t.insert("type".into(), "stdio".into());
            t.insert("command".into(), first.as_str().into());
            if target.len() > 1 {
                t.insert(
                    "args".into(),
                    target[1..]
                        .iter()
                        .map(|a| a.as_str().into())
                        .collect::<Vec<serde_yaml::Value>>()
                        .into(),
                );
            }
            t.into()
        };
    entry.insert("transport".into(), transport);
    if !env.is_empty() {
        entry.insert("env".into(), pairs(env, "--env")?.into());
    }
    if oauth {
        entry.insert("oauth".into(), serde_yaml::Mapping::new().into());
    }
    let entry = serde_yaml::Value::Mapping(entry);
    // The entry must read back as a server, as the configuration loader
    // will read it.
    serde_yaml::from_value::<crate::McpServerConfig>(entry.clone())
        .map_err(|e| anyhow::anyhow!("not a valid MCP server entry: {e}"))?;
    Ok(entry)
}

fn valid_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "a server name is letters, digits, `_` and `-`, so its tools are named `{name}-<tool>`"
    );
    Ok(())
}

pub fn config_file(scope: sven_config::scope::Scope) -> anyhow::Result<std::path::PathBuf> {
    let here = std::env::current_dir()?;
    sven_config::scope::scope_file(scope, &here)
        .ok_or_else(|| anyhow::anyhow!("no {scope} configuration file can be named here"))
}

fn server_path(name: &str) -> [String; 2] {
    ["mcp_servers".to_string(), name.to_string()]
}

/// Write `entry` as server `name` into the `scope` configuration file, which
/// must not have one by that name yet. Returns the file.
///
/// # Errors
///
/// The name is not a valid tool prefix, the server exists, or the file cannot
/// be read or written.
pub fn add(scope: Scope, name: &str, entry: serde_yaml::Value) -> anyhow::Result<PathBuf> {
    valid_name(name)?;
    let file = config_file(scope)?;
    let path = server_path(name);
    let document = scope::read_file(&file)?;
    anyhow::ensure!(
        scope::value_at(&document, &path).is_none(),
        "{} already has an MCP server '{name}'; remove it first",
        file.display()
    );
    let document = scope::with_value_at(document, &path, entry).map_err(anyhow::Error::msg)?;
    scope::write_file(&file, &document)?;
    Ok(file)
}

/// Remove server `name` from the `scope` configuration file. Returns the
/// file.
///
/// # Errors
///
/// The file has no such server, or cannot be read or written.
pub fn remove(scope: Scope, name: &str) -> anyhow::Result<PathBuf> {
    let file = config_file(scope)?;
    let (document, removed) = scope::without_value_at(scope::read_file(&file)?, &server_path(name));
    anyhow::ensure!(removed, "{} has no MCP server '{name}'", file.display());
    scope::write_file(&file, &document)?;
    Ok(file)
}
