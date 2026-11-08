// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The MCP servers a harness package declares: `mcp.yaml` beside its main
//! workflow. A package that needs a toolkit (a decompiler, a CAD kernel)
//! offers it to its agents as an MCP server of its own instead of a command
//! line the model must drive through the shell.
//!
//! This module reads and checks the declaration and turns each server into
//! a configuration. A local `sven harness run` starts them as it runs
//! everything else, as the person running it; a served run
//! (`harness work`) confines each the way it confines the agent's commands
//! (`super::work_mcp`). Either way each is `required`: the run's session
//! waits for it, and a run whose server does not start or offers no tool
//! fails before its first step.
//!
//! ```yaml
//! servers:
//!   rea:
//!     command: rea                 # a program in the package's bin or on PATH
//!     args: [mcp]
//!     env: { REA_MCP_OUTPUT_SCHEMA_PROFILE: "off" }
//!     read_only: [/proc]           # absolute; besides the system set
//!     timeout_secs: 900            # the longest one call may take
//!     tools: { "*": allow, gdb_console: deny }
//! ```
//!
//! The declaration can only narrow what a server reaches: it cannot name a
//! writable directory, an HTTP server, a variable read from the host's
//! environment, or a policy that waits for a person nobody may be there to
//! be.
//!
//! Swedish Embedded AB implements solutions for giving agents whole software
//! toolkits without giving them the host for its clients. If your team needs
//! expertise in agent sandboxing then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sven_mcp_client::{McpServerConfig, McpToolPolicy, McpTransport};

/// The file a package declares its servers in, beside its main workflow.
pub(crate) const DECLARATION: &str = "mcp.yaml";
const MAX_SERVERS: usize = 4;
const MAX_ARGS: usize = 64;
const MAX_ENV: usize = 64;
const MAX_READ_ONLY: usize = 16;
const MAX_TIMEOUT_SECS: u64 = 3600;
/// Variables sven sets itself for every server.
const RESERVED_ENV: &[&str] = &["PATH", "HOME", "TMPDIR"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    servers: BTreeMap<String, Declared>,
}

/// One declared server, checked.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Declared {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Absolute directories the server reads besides the system set.
    #[serde(default)]
    pub read_only: Vec<PathBuf>,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default)]
    tools: HashMap<String, McpToolPolicy>,
}

fn default_timeout() -> u64 {
    300
}

/// The servers declared in `dir/mcp.yaml`, checked, by name; none when the
/// package declares none.
///
/// # Errors
///
/// The declaration is malformed or asks for more than a package may.
pub(crate) fn read(dir: &Path) -> Result<Vec<(String, Declared)>> {
    let path = dir.join(DECLARATION);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| path.display().to_string()),
    };
    let declaration: Declaration =
        serde_yaml::from_str(&text).with_context(|| format!("the package's {DECLARATION}"))?;
    if declaration.servers.len() > MAX_SERVERS {
        bail!("a package declares at most {MAX_SERVERS} MCP servers");
    }
    declaration
        .servers
        .into_iter()
        .map(|(name, declared)| {
            check(&name, &declared)
                .with_context(|| format!("MCP server `{name}` of the package"))?;
            Ok((name, declared))
        })
        .collect()
}

fn check(name: &str, declared: &Declared) -> Result<()> {
    // A server's tools are named `<server>-<tool>`: its name holds no dash.
    let name_ok = !name.is_empty()
        && name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !name_ok {
        bail!("a server's name is 1-32 lowercase letters, digits and underscores");
    }
    let command_ok = !declared.command.is_empty()
        && declared.command.len() <= 128
        && declared
            .command
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'));
    if !command_ok {
        bail!("`command` is the name of a program in the package's bin or on PATH, not a path");
    }
    if declared.args.len() > MAX_ARGS
        || declared
            .args
            .iter()
            .any(|a| a.len() > 1024 || a.contains('\0'))
    {
        bail!("at most {MAX_ARGS} arguments of at most 1024 bytes");
    }
    if declared.env.len() > MAX_ENV {
        bail!("at most {MAX_ENV} environment variables");
    }
    for (key, value) in &declared.env {
        let key_ok = key.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
            && key
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !key_ok || RESERVED_ENV.contains(&key.as_str()) || key.starts_with("LD_") {
            bail!("`{key}` is not a variable a package may set");
        }
        // Nothing is read from the host's own environment: no expansion.
        if value.contains('$') || value.contains('\0') || value.len() > 4096 {
            bail!("`{key}` must be a literal value of at most 4096 bytes");
        }
    }
    if declared.read_only.len() > MAX_READ_ONLY {
        bail!("at most {MAX_READ_ONLY} read-only directories");
    }
    for dir in &declared.read_only {
        let plain = dir.is_absolute()
            && dir
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
        if !plain || dir == Path::new("/") {
            bail!("`{}` is not an absolute directory below /", dir.display());
        }
    }
    if !(1..=MAX_TIMEOUT_SECS).contains(&declared.timeout_secs) {
        bail!("`timeout_secs` is 1 to {MAX_TIMEOUT_SECS}");
    }
    if declared.tools.values().any(|p| *p == McpToolPolicy::Ask) {
        bail!("a harness run may have nobody to ask: a tool is `allow` or `deny`");
    }
    Ok(())
}

/// Where a server lives: a program of the package's `bin` is started from
/// there, and its `HOME` and `TMPDIR` are directories of the run.
pub(crate) struct Home<'a> {
    pub bin: Option<&'a Path>,
    pub home: &'a Path,
    pub tmp: &'a Path,
}

/// The configuration of `declared`: started with a clean environment (what
/// it declares, the host's `PATH`, its own `HOME` and `TMPDIR`), required
/// by the run.
///
/// # Errors
///
/// `HOME` cannot be made.
pub(crate) fn config(declared: Declared, at: &Home<'_>) -> Result<McpServerConfig> {
    std::fs::create_dir_all(at.home).with_context(|| at.home.display().to_string())?;
    // The package's own program wins; any other is found on the PATH the
    // server inherits.
    let command = match at.bin.map(|bin| bin.join(&declared.command)) {
        Some(own) if own.is_file() => own.display().to_string(),
        _ => declared.command,
    };
    let mut env: HashMap<String, String> = declared.env.into_iter().collect();
    env.insert("HOME".into(), at.home.display().to_string());
    env.insert("TMPDIR".into(), at.tmp.display().to_string());
    Ok(McpServerConfig {
        transport: McpTransport::Stdio {
            command,
            args: declared.args,
        },
        env,
        timeout_secs: declared.timeout_secs,
        tools: declared.tools,
        required: true,
        ..McpServerConfig::default()
    })
}

/// The servers of a local `sven harness run` of `harness` (the workflow
/// file, or the directory holding it), started as the person running it
/// with their home under `state`.
///
/// # Errors
///
/// See [`read`] and [`config`].
pub(crate) fn local_servers(
    harness: &Path,
    state: &Path,
) -> Result<HashMap<String, McpServerConfig>> {
    let dir = if harness.is_dir() {
        harness
    } else {
        harness.parent().unwrap_or(Path::new("."))
    };
    let bin = dir.join("bin");
    let bin = bin.is_dir().then_some(bin);
    read(dir)?
        .into_iter()
        .map(|(name, declared)| {
            let home = state.join(format!("mcp-{name}"));
            let config = config(
                declared,
                &Home {
                    bin: bin.as_deref(),
                    home: &home,
                    tmp: &home,
                },
            )?;
            Ok((name, config))
        })
        .collect()
}

/// Puts `servers` in `config`: with a toolkit's hundreds of tools and large
/// results, its agents load the definitions they need and older results
/// shrink to a reference they can expand.
pub(crate) fn apply(
    config: &mut sven_bootstrap::Config,
    servers: HashMap<String, McpServerConfig>,
) {
    if !servers.is_empty() {
        config.tools.disclosure.mode = sven_tool_registry::Disclosure::Deferred;
        config.context.assembly.mode = sven_bootstrap::AssemblyMode::Demote;
    }
    config.mcp_servers = servers;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(yaml: &str) -> Result<Vec<(String, Declared)>> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DECLARATION), yaml).unwrap();
        read(dir.path())
    }

    #[test]
    fn a_package_without_a_declaration_has_no_servers() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_declaration_can_only_narrow_what_a_server_reaches() {
        let refused = [
            ("servers: {Rea: {command: rea}}", "name"),
            ("servers: {rea: {command: /usr/bin/rea}}", "not a path"),
            (
                "servers: {rea: {command: rea, env: {TOKEN: '${HOST_TOKEN}'}}}",
                "literal",
            ),
            (
                "servers: {rea: {command: rea, env: {PATH: /evil}}}",
                "may set",
            ),
            (
                "servers: {rea: {command: rea, env: {LD_PRELOAD: x.so}}}",
                "may set",
            ),
            (
                "servers: {rea: {command: rea, read_only: [relative]}}",
                "absolute",
            ),
            (
                "servers: {rea: {command: rea, read_only: ['/usr/../etc']}}",
                "absolute",
            ),
            (
                "servers: {rea: {command: rea, read_only: ['/']}}",
                "absolute",
            ),
            (
                "servers: {rea: {command: rea, tools: {'*': ask}}}",
                "nobody to ask",
            ),
            (
                "servers: {rea: {command: rea, timeout_secs: 0}}",
                "timeout_secs",
            ),
            (
                "servers: {rea: {transport: {type: http, url: 'https://x'}}}",
                "unknown field",
            ),
        ];
        for (yaml, why) in refused {
            let error = format!("{:#}", declared(yaml).unwrap_err());
            assert!(error.contains(why), "{yaml}: {error}");
        }
    }

    #[test]
    fn a_local_run_starts_a_declared_server_with_a_clean_environment_and_requires_it() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("package");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::write(
            package.join(DECLARATION),
            "servers: {rea: {command: rea, args: [mcp], env: {REA_MCP_OUTPUT_SCHEMA_PROFILE: 'off'}, timeout_secs: 900}}",
        )
        .unwrap();
        std::fs::write(package.join("harness.yaml"), "").unwrap();
        let state = dir.path().join("state");
        let servers = local_servers(&package.join("harness.yaml"), &state).unwrap();
        let rea = &servers["rea"];
        assert!(rea.required);
        assert!(rea.restrict_spawn.is_none());
        assert_eq!(rea.timeout_secs, 900);
        assert_eq!(PathBuf::from(&rea.env["HOME"]), state.join("mcp-rea"));
        assert!(
            !rea.env.contains_key("PATH"),
            "the server inherits the host's PATH"
        );
        assert!(matches!(&rea.transport, McpTransport::Stdio { command, .. } if command == "rea"));
        std::fs::write(package.join("bin").join("rea"), "").unwrap();
        let own = local_servers(&package.join("harness.yaml"), &state).unwrap();
        let own_command = package.join("bin").join("rea").display().to_string();
        assert!(
            matches!(&own["rea"].transport, McpTransport::Stdio { command, .. } if *command == own_command)
        );
    }
}
