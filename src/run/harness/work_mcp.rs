// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The MCP servers a served run's package declares
//! ([`super::package_mcp`]), confined.
//!
//! The worker is trusted and the server is not: whatever a server does, it
//! does on the agent's behalf with input the run was given. So each server
//! runs under the same kernel confinement as the agent's commands
//! ([`super::work::agent_confinement`]), reaching the workspace and a private
//! home in the agent's tmp for writing, its declared read-only directories,
//! the package's `bin`, the `PATH` and the system set for reading, and no
//! network; it gets a clean environment (only what it declares, its `PATH`,
//! `HOME` and `TMPDIR`), never the worker's. A read-only directory that
//! would reach the run's private directory is refused.
//!
//! A package may open `/proc` to its server for reading (`read_only:
//! [/proc]`; a toolkit that checks which processes it started needs `ps`):
//! that shows the command lines of the run's processes, never the worker's
//! memory, environment or open files, which its non-dumpable state keeps
//! from every process of the same user.
//!
//! Swedish Embedded AB implements solutions for giving agents whole software
//! toolkits without giving them the host for its clients. If your team needs
//! expertise in agent sandboxing then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use sven_mcp_client::{McpServerConfig, SpawnRestriction};

use super::package_mcp::{self, Home};

/// What the worker knows when it starts a package's servers.
pub(crate) struct Place<'a> {
    /// The run's workspace: where the agent and the servers write.
    pub workspace: &'a Path,
    /// The run's private directory, out of every server's reach.
    pub scratch: &'a Path,
    /// The package's `bin`, first on each server's `PATH`.
    pub bin: Option<&'a Path>,
}

/// The servers declared in `dir/mcp.yaml`, confined, ready to put in the
/// engine's configuration; none when the package declares none.
///
/// # Errors
///
/// The declaration is malformed, asks for more than a package may (a
/// read-only directory reaching the run's private directory included), or a
/// confinement cannot be built on this kernel.
pub(crate) fn declared_servers(
    dir: &Path,
    place: &Place<'_>,
) -> Result<HashMap<String, McpServerConfig>> {
    package_mcp::read(dir)?
        .into_iter()
        .map(|(name, declared)| {
            let config = confined(&name, declared, place)
                .with_context(|| format!("MCP server `{name}` of the package"))?;
            Ok((name, config))
        })
        .collect()
}

fn confined(
    name: &str,
    declared: package_mcp::Declared,
    place: &Place<'_>,
) -> Result<McpServerConfig> {
    for dir in &declared.read_only {
        if dir.starts_with(place.scratch) || place.scratch.starts_with(dir) {
            bail!(
                "`{}` would reach the run's private directory",
                dir.display()
            );
        }
    }
    let tmp = place.scratch.join("agent-tmp");
    let home = tmp.join(format!("mcp-{name}"));
    let mut confinement =
        super::work::agent_confinement(place.workspace, place.scratch, place.bin)?;
    confinement
        .read_exec
        .extend(declared.read_only.iter().cloned());
    // Built once here, where a failure can refuse the run; applied in the
    // child of every start.
    confinement.prepare().map_err(|e| anyhow!(e))?;
    let mut config = package_mcp::config(
        declared,
        &Home {
            bin: place.bin,
            home: &home,
            tmp: &tmp,
        },
    )?;
    // As for the agent's commands: the package's `bin` first on `PATH`.
    let path = place
        .bin
        .map(Path::to_path_buf)
        .into_iter()
        .chain(sven_tools_exec::search_path_dirs());
    let path = std::env::join_paths(path).context("the server's PATH")?;
    config
        .env
        .insert("PATH".into(), path.to_string_lossy().into_owned());
    config.restrict_spawn = Some(SpawnRestriction(Arc::new(move |command| {
        confinement.restrict(command)
    })));
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use sven_mcp_client::McpTransport;
    use sven_tools_exec::confine::CommandConfinement;

    fn place(dir: &Path) -> (PathBuf, PathBuf) {
        let workspace = dir.join("workspace");
        let scratch = dir.join("scratch");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        (workspace, scratch)
    }

    fn declared(dir: &Path, yaml: &str) -> Result<HashMap<String, McpServerConfig>> {
        let (workspace, scratch) = place(dir);
        let package = dir.join("package");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join(package_mcp::DECLARATION), yaml).unwrap();
        declared_servers(
            &package,
            &Place {
                workspace: &workspace,
                scratch: &scratch,
                bin: None,
            },
        )
    }

    #[test]
    fn a_read_only_directory_never_reaches_the_runs_private_directory() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        let yaml = format!(
            "servers: {{rea: {{command: rea, read_only: ['{}']}}}}",
            scratch.join("package").display()
        );
        let error = format!("{:#}", declared(dir.path(), &yaml).unwrap_err());
        assert!(error.contains("private directory"), "{error}");
    }

    /// Only where the kernel confines: elsewhere every declaration is refused.
    #[test]
    fn a_declared_server_gets_a_clean_confined_start() {
        if CommandConfinement::require_kernel_support().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let servers = declared(
            dir.path(),
            "servers: {rea: {command: rea, args: [mcp], env: {REA_MCP_OUTPUT_SCHEMA_PROFILE: 'off'}, read_only: [/usr/share/ghidra], timeout_secs: 900, tools: {'*': allow, gdb_console: deny}}}",
        )
        .unwrap();
        let rea = &servers["rea"];
        assert!(rea.restrict_spawn.is_some());
        assert_eq!(rea.timeout_secs, 900);
        assert_eq!(rea.env["REA_MCP_OUTPUT_SCHEMA_PROFILE"], "off");
        let scratch = dir.path().join("scratch");
        assert_eq!(PathBuf::from(&rea.env["TMPDIR"]), scratch.join("agent-tmp"));
        assert_eq!(
            PathBuf::from(&rea.env["HOME"]),
            scratch.join("agent-tmp/mcp-rea")
        );
        assert!(matches!(&rea.transport, McpTransport::Stdio { command, .. } if command == "rea"));
    }

    /// A real toolkit under the confinement: REA with Ghidra decompiles a
    /// binary in the workspace while reaching nothing else. Its kit (Node,
    /// `rea-agents`, Ghidra, a JDK and an ELF `target`, all pinned) is
    /// fetched by `make test/harness/toolkit`, which runs this.
    #[tokio::test]
    #[ignore = "needs the pinned REA kit: make test/harness/toolkit"]
    async fn a_confined_toolkit_decompiles_a_binary_in_the_workspace() {
        let kit = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/kits/rea");
        assert!(
            kit.join(".complete").is_file(),
            "no kit at {}: run make test/harness/toolkit",
            kit.display()
        );
        CommandConfinement::require_kernel_support().unwrap();
        // Ghidra refuses a project path with a dot-prefixed element (as
        // tempfile's default `.tmpXXXX` is); a worker's scratch has none.
        let dir = tempfile::Builder::new()
            .prefix("rea-kit-")
            .tempdir()
            .unwrap();
        let (workspace, scratch) = place(dir.path());
        std::fs::copy(kit.join("target"), workspace.join("target")).unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(kit.join("node/bin/node"), bin.join("node")).unwrap();
        std::os::unix::fs::symlink(kit.join("rea/node_modules/.bin/rea"), bin.join("rea")).unwrap();
        let package = dir.path().join("package");
        std::fs::create_dir_all(&package).unwrap();
        let read_only: Vec<String> = ["node", "rea", "ghidra", "jdk"]
            .iter()
            .map(|d| {
                format!(
                    "'{}'",
                    std::fs::canonicalize(kit.join(d)).unwrap().display()
                )
            })
            .collect();
        std::fs::write(
            package.join(package_mcp::DECLARATION),
            format!(
                "servers: {{rea: {{command: rea, args: [mcp], timeout_secs: 900, tools: {{'*': allow}}, read_only: [{}], env: {{REA_MCP_OUTPUT_SCHEMA_PROFILE: 'off', GHIDRA_INSTALL_DIR: '{}', JAVA_HOME: '{}'}}}}}}",
                read_only.join(", ") + ", /proc",
                std::fs::canonicalize(kit.join("ghidra")).unwrap().display(),
                std::fs::canonicalize(kit.join("jdk")).unwrap().display(),
            ),
        )
        .unwrap();
        let servers = declared_servers(
            &package,
            &Place {
                workspace: &workspace,
                scratch: &scratch,
                bin: Some(&bin),
            },
        )
        .unwrap();
        assert!(servers["rea"].required);
        let (events, _ignored) = tokio::sync::mpsc::channel(64);
        let manager = sven_mcp_client::McpManager::new(servers, events, false);
        manager.connect("rea").await.unwrap();
        let target = workspace.join("target").display().to_string();
        let opened = manager
            .call_tool("rea", "open_binary", &serde_json::json!({ "path": target }))
            .await
            .unwrap();
        assert_ne!(opened.is_error, Some(true), "{opened:?}");
        let decompiled = manager
            .call_tool(
                "rea",
                "analyze_function",
                &serde_json::json!({ "procedure": "main", "detail": "complete" }),
            )
            .await
            .unwrap();
        let text = serde_json::to_string(&decompiled).unwrap();
        assert_ne!(
            decompiled.is_error,
            Some(true),
            "{}",
            &text[..text.len().min(2000)]
        );
        assert!(
            text.contains("pseudocode"),
            "{}",
            &text[..text.len().min(2000)]
        );
        // Outside the workspace, nothing: the worker's private directory is
        // out of reach of the server too.
        std::fs::write(scratch.join("secret"), b"x").unwrap();
        let refused = manager
            .call_tool(
                "rea",
                "open_binary",
                &serde_json::json!({ "path": scratch.join("secret").display().to_string() }),
            )
            .await
            .unwrap();
        assert_eq!(refused.is_error, Some(true), "{refused:?}");
    }
}
