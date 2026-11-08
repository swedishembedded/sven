// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The MCP servers a served harness's package declares: `mcp.yaml` beside
//! its main workflow. A package that needs a toolkit (a decompiler, a CAD
//! kernel) offers it to the run's agents as an MCP server of its own instead
//! of a command line the model must drive through the shell.
//!
//! The worker is trusted and the server is not: whatever a server does, it
//! does on the agent's behalf with input the run was given. So each server
//! runs under the same kernel confinement as the agent's commands
//! ([`super::work::agent_confinement`]), reaching the workspace and a private
//! home in the agent's tmp for writing, its declared read-only directories,
//! the package's `bin`, the `PATH` and the system set for reading, and no
//! network; it gets a clean environment (only what it declares, its `PATH`,
//! `HOME` and `TMPDIR`), never the worker's. The declaration can only narrow
//! that: it cannot name a writable directory, an HTTP server, a variable
//! read from the worker's environment, or a policy that waits for a person
//! nobody is there to be.
//!
//! A package may open `/proc` to its server for reading (`read_only:
//! [/proc]`; a toolkit that checks which processes it started needs `ps`):
//! that shows the command lines of the run's processes, never the worker's
//! memory, environment or open files, which its non-dumpable state keeps
//! from every process of the same user.
//!
//! Each server is `required`: the run's session waits for it, and a run
//! whose server does not start or offers no tool fails before its first
//! step instead of going on without the toolkit it was built for.
//!
//! ```yaml
//! servers:
//!   rea:
//!     command: rea                 # a program in the package's bin or on PATH
//!     args: [mcp]
//!     env: { REA_MCP_OUTPUT_SCHEMA_PROFILE: "off" }
//!     read_only: [/proc]          # absolute; besides the system set
//!     timeout_secs: 900            # the longest one call may take
//!     tools: { "*": allow, gdb_console: deny }
//! ```
//!
//! Swedish Embedded AB implements solutions for giving agents whole software
//! toolkits without giving them the host for its clients. If your team needs
//! expertise in agent sandboxing then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sven_mcp_client::{McpServerConfig, McpToolPolicy, McpTransport, SpawnRestriction};

/// The file a package declares its servers in, beside its main workflow.
pub(crate) const DECLARATION: &str = "mcp.yaml";
const MAX_SERVERS: usize = 4;
const MAX_ARGS: usize = 64;
const MAX_ENV: usize = 64;
const MAX_READ_ONLY: usize = 16;
const MAX_TIMEOUT_SECS: u64 = 3600;
/// Variables the worker sets itself for every server.
const RESERVED_ENV: &[&str] = &["PATH", "HOME", "TMPDIR"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    servers: BTreeMap<String, Declared>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declared {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    read_only: Vec<PathBuf>,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default)]
    tools: HashMap<String, McpToolPolicy>,
}

fn default_timeout() -> u64 {
    300
}

/// What the worker knows when it starts a package's servers.
pub(crate) struct Place<'a> {
    /// The run's workspace: where the agent and the servers write.
    pub workspace: &'a Path,
    /// The run's private directory, out of every server's reach.
    pub scratch: &'a Path,
    /// The package's `bin`, first on each server's `PATH`.
    pub bin: Option<&'a Path>,
}

/// The servers declared in `dir/mcp.yaml`, ready to put in the engine's
/// configuration; none when the package declares none.
///
/// # Errors
///
/// The declaration is malformed, asks for more than a package may, or a
/// confinement cannot be built on this kernel.
pub(crate) fn declared_servers(
    dir: &Path,
    place: &Place<'_>,
) -> Result<HashMap<String, McpServerConfig>> {
    let path = dir.join(DECLARATION);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
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
            let config = server(&name, declared, place)
                .with_context(|| format!("MCP server `{name}` of the package"))?;
            Ok((name, config))
        })
        .collect()
}

fn server(name: &str, declared: Declared, place: &Place<'_>) -> Result<McpServerConfig> {
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
        // Nothing is read from the worker's own environment: no expansion.
        if value.contains('$') || value.contains('\0') || value.len() > 4096 {
            bail!("`{key}` must be a literal value of at most 4096 bytes");
        }
    }
    if declared.read_only.len() > MAX_READ_ONLY {
        bail!("at most {MAX_READ_ONLY} read-only directories");
    }
    for dir in &declared.read_only {
        read_only_ok(dir, place.scratch)?;
    }
    if !(1..=MAX_TIMEOUT_SECS).contains(&declared.timeout_secs) {
        bail!("`timeout_secs` is 1 to {MAX_TIMEOUT_SECS}");
    }
    if declared.tools.values().any(|p| *p == McpToolPolicy::Ask) {
        bail!("a served run has nobody to ask: a tool is `allow` or `deny`");
    }

    let tmp = place.scratch.join("agent-tmp");
    let home = tmp.join(format!("mcp-{name}"));
    std::fs::create_dir_all(&home).with_context(|| home.display().to_string())?;
    let mut confinement =
        super::work::agent_confinement(place.workspace, place.scratch, place.bin)?;
    confinement.read_exec.extend(declared.read_only);
    // Built once here, where a failure can refuse the run; applied in the
    // child of every start.
    confinement.prepare().map_err(|e| anyhow!(e))?;
    let restrict = SpawnRestriction(Arc::new(move |command| confinement.restrict(command)));

    let path = place
        .bin
        .map(Path::to_path_buf)
        .into_iter()
        .chain(sven_tools_exec::search_path_dirs())
        .collect::<Vec<_>>();
    let mut env: HashMap<String, String> = declared.env.into_iter().collect();
    env.insert(
        "PATH".into(),
        std::env::join_paths(path)
            .context("the server's PATH")?
            .to_string_lossy()
            .into_owned(),
    );
    env.insert("HOME".into(), home.display().to_string());
    env.insert("TMPDIR".into(), tmp.display().to_string());
    Ok(McpServerConfig {
        transport: McpTransport::Stdio {
            command: declared.command,
            args: declared.args,
        },
        env,
        timeout_secs: declared.timeout_secs,
        tools: declared.tools,
        restrict_spawn: Some(restrict),
        // Started once, by the run's own session, which refuses to begin
        // without it: a second instance would only race the first.
        required: true,
        ..McpServerConfig::default()
    })
}

/// A read-only directory a package may add: absolute, plain, not the root
/// and nothing of the run's private directory.
fn read_only_ok(dir: &Path, scratch: &Path) -> Result<()> {
    let plain = dir.is_absolute()
        && dir
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    if !plain || dir == Path::new("/") {
        bail!("`{}` is not an absolute directory below /", dir.display());
    }
    if dir.starts_with(scratch) || scratch.starts_with(dir) {
        bail!(
            "`{}` would reach the run's private directory",
            dir.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        std::fs::write(package.join(DECLARATION), yaml).unwrap();
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
    fn a_package_without_a_declaration_has_no_servers() {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, scratch) = place(dir.path());
        let none = declared_servers(
            dir.path(),
            &Place {
                workspace: &workspace,
                scratch: &scratch,
                bin: None,
            },
        )
        .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn a_declaration_can_only_narrow_what_a_server_reaches() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        let refused = [
            ("servers: {Rea: {command: rea}}", "name"),
            ("servers: {rea: {command: /usr/bin/rea}}", "not a path"),
            (
                "servers: {rea: {command: rea, env: {TOKEN: '${WHALE_TOKEN}'}}}",
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
            let error = format!("{:#}", declared(dir.path(), yaml).unwrap_err());
            assert!(error.contains(why), "{yaml}: {error}");
        }
        let scratch_dir = format!(
            "servers: {{rea: {{command: rea, read_only: ['{}']}}}}",
            scratch.join("package").display()
        );
        let error = format!("{:#}", declared(dir.path(), &scratch_dir).unwrap_err());
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
            package.join(DECLARATION),
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
