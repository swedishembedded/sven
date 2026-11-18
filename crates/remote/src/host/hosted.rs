// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for letting a website's visitors
// talk to an agent that cannot touch the server it runs on for its clients.
// If your team needs expertise in sandboxed agent hosting then you can
// procure our services by sending an email to info@swedishembedded.com.

//! What a hosted host (`sven rc --hosted`) allows its sessions, and what a
//! session on either kind of host is told about where it works.

use std::path::{Path, PathBuf};

use sven_bootstrap::{Config, RuntimeContext};
use sven_tool_api::PathScope;

/// The only tools a hosted host's sessions have (`sven rc --hosted`): none
/// of them runs anything on the machine the host runs on, and none reaches a
/// file outside the conversation's own confined directory.
pub const HOSTED_TOOLS: &[&str] = &[
    "web_search",
    "web_fetch",
    "skill",
    // Models of every modality run as the person on the sven service, their
    // outputs the conversation's files: no file system, shell or network
    // beyond that one service, whose key the host already holds.
    "models",
    "model_schema",
    "run_model",
    // Documents (PDFs, pictures of pages) to Markdown through OCR models:
    // read from the conversation's files or its directory, parsed only in
    // confined worker processes (no network, nothing writable, hard
    // memory, CPU and time limits; `ocr.isolate`), the Markdown published as
    // a file of the conversation.
    "document_locate",
    "document_convert",
    // Sub-agents: this binary again, inside the same confinement, held to
    // this allow list (without `task`) and this session's endpoints and key,
    // handed over on its stdin.
    "task",
];

/// The system prompt of a hosted conversation when the site configures none
/// (`agent.system_prompt` in the file `sven --config` reads).
pub const HOSTED_SYSTEM_PROMPT: &str = "You are sven, a helpful assistant. Answer clearly and \
     concisely, and say so when you are not sure. You can search the web and read public web \
     pages; say which sources you used. You can run other models (image, speech, vision, music) \
     with `models`, `model_schema` and `run_model`; what they make is shown in the conversation, \
     and you pass a file on to the next model by its handle. You have no access to the person's \
     computer or files beyond those of this conversation.";

/// What a session works with. On a person's computer: the project it runs in,
/// whose root, git state and context file the built-in prompt describes, with
/// a configured `agent.system_prompt` as its identity. Hosted: the configured
/// prompt (or [`HOSTED_SYSTEM_PROMPT`]) and nothing else, since the working
/// directory is the site's own sandbox, not something to tell a visitor about.
///
/// Its tools start in `working_dir` and are held to nothing by sven: what a
/// session can reach is the system's to decide (the person's own machine,
/// a container, the kernel's confinement of a hosted host).
///
/// # Errors
///
/// A hosted conversation's directory cannot be resolved.
pub(super) fn session_runtime(
    config: &Config,
    working_dir: &Path,
    hosted: bool,
    shared_workdir: bool,
) -> anyhow::Result<RuntimeContext> {
    let mut runtime = RuntimeContext::auto_detect_at(Some(working_dir.to_path_buf()));
    runtime.project_root = Some(working_dir.to_path_buf());
    // sven holds no session's tools to a directory: a person's computer
    // and a container are the person's, and a hosted host is held by the
    // kernel to its person's sandbox. A hosted conversation works in a
    // directory of its own below the host's, so its relative paths start
    // there.
    if shared_workdir {
        runtime.path_scope = PathScope::anchored(working_dir)?;
    }
    if hosted {
        runtime.no_system = true;
        runtime.system_prompt_override = Some(
            config
                .agent
                .system_prompt
                .clone()
                .unwrap_or_else(|| HOSTED_SYSTEM_PROMPT.to_string()),
        );
    } else {
        runtime.system_prompt_override = config.agent.system_prompt.clone();
    }
    Ok(runtime)
}

/// Where a hosted host keeps each conversation's files, below its working
/// directory.
const CONVERSATIONS: &str = ".sven/conversations";

/// The directory conversation `id`'s tools, attachments and files live in,
/// created when missing. Hosted, a directory of its own: the host's working
/// directory is shared by all of its person's conversations, and one
/// conversation (or someone it is shared with) must not reach another's
/// files by path. On a person's computer, the directory `sven rc` runs in.
///
/// # Errors
///
/// It cannot be created.
pub(super) fn conversation_dir(
    working_dir: &Path,
    hosted: bool,
    id: uuid::Uuid,
) -> anyhow::Result<PathBuf> {
    if !hosted {
        return Ok(working_dir.to_path_buf());
    }
    let dir = working_dir.join(CONVERSATIONS).join(id.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// A closed conversation's own directory removed with it, where the working
/// directory is `shared`. Nothing else is ever removed: a person's computer
/// and a container's workspace are theirs.
pub(super) fn forget_conversation_files(working_dir: &Path, shared: bool, id: uuid::Uuid) {
    if !shared {
        return;
    }
    let dir = working_dir.join(CONVERSATIONS).join(id.to_string());
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("conversation {id}: its files were not removed: {e}"),
    }
}

/// Where conversation `id` keeps its temporary files, below `cache` (the
/// person's cache directory: `~/.cache` on Linux, `~/Library/Caches` on
/// macOS), created private to the person when missing. The shared `/tmp`
/// holds other programs' files and sockets; this is the conversation's own,
/// and it stays while the conversation does, across a restarted host or a
/// resumed environment.
///
/// # Errors
///
/// It cannot be created.
pub(super) fn scratch_dir(cache: &Path, id: uuid::Uuid) -> anyhow::Result<PathBuf> {
    let dir = cache.join("sven").join("scratch").join(id.to_string());
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(&dir)
        .map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// A closed conversation's scratch directory removed with it.
pub(super) fn forget_scratch(cache: &Path, id: uuid::Uuid) {
    let dir = cache.join("sven").join("scratch").join(id.to_string());
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("conversation {id}: its scratch files were not removed: {e}"),
    }
}

/// What a remotely driven session on a person's computer may reach unless
/// the person asks for more (`sven rc --wide-access`). The server chooses
/// the tool calls and relays the approvals, so neither is a barrier:
/// `web_fetch` reaches only the public internet (never this machine's
/// network or a cloud metadata service), and gdb runs no host program the
/// model spells out. Files and commands are the system's to confine (a
/// container); `tools.only` and `tools.disabled` remove tools.
pub fn restrict_remote_session(config: &mut Config) {
    config.tools.web.public_only = true;
    config.tools.gdb.allow_host_commands = false;
}

/// What a hosted host's sessions may do, applied to its configuration: only
/// [`HOSTED_TOOLS`] (an allow list, so a tool added later is never offered
/// unless named there), and `web_fetch` reaching only the public internet,
/// never the server's own network (its services, its database, a cloud
/// metadata service). No configured MCP server is started or reached: an
/// allow list hides a server's tools, but a stdio server would still run on
/// the host and an HTTP one still be called. Tools are disclosed on demand
/// (the discovery tool), whatever the configuration says, and documents are
/// parsed only in confined workers (`ocr.isolate`).
pub fn restrict_to_hosted(config: &mut Config) {
    config.mcp_servers.clear();
    config.tools.only = Some(
        HOSTED_TOOLS
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
    );
    config.tools.web.public_only = true;
    // Its paths are the server's: where its sandbox is says how the server
    // lays out every account's.
    config.tools.hide_locations = true;
    // Connector tools change while sessions run: disclosed on demand, a
    // change leaves what each request carries, and the cached prefix, alone.
    config.tools.disclosure.mode = sven_bootstrap::Disclosure::Deferred;
    // A document someone sent is parsed only in a confined worker.
    config.ocr.isolate = Some(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// A hosted session's connectors may grow to hundreds of tools and change
    /// while it runs: it always discloses them on demand, so a change leaves
    /// what each request carries alone, even where a configuration says Full.
    /// A hosted host's skill tool never names where anything is on the
    /// server, whatever its configuration file says.
    #[test]
    fn a_hosted_host_hides_where_its_skills_are() {
        let mut config: Config = serde_yaml::from_str("tools: {}\n").unwrap();
        assert!(!config.tools.hide_locations);
        restrict_to_hosted(&mut config);
        assert!(config.tools.hide_locations);
    }

    #[test]
    fn a_hosted_host_discloses_tools_on_demand_whatever_its_configuration_says() {
        let mut config: Config =
            serde_yaml::from_str("tools:\n  disclosure:\n    mode: full\n").unwrap();
        assert_eq!(
            config.tools.disclosure.mode,
            sven_bootstrap::Disclosure::Full
        );
        restrict_to_hosted(&mut config);
        assert_eq!(
            config.tools.disclosure.mode,
            sven_bootstrap::Disclosure::Deferred
        );
    }

    /// A configured MCP server would run (stdio) or be reached (HTTP) from
    /// the hosted host even with its tools hidden by the allow list.
    #[test]
    fn a_hosted_host_starts_and_reaches_no_configured_mcp_server() {
        let mut config: Config = serde_yaml::from_str(
            "mcp_servers:\n  local: {command: /bin/true}\n  remote: {url: https://mcp.example.com}\n",
        )
        .unwrap();
        assert_eq!(config.mcp_servers.len(), 2);
        restrict_to_hosted(&mut config);
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn a_remotely_driven_session_gets_a_public_only_web_and_no_gdb_host_commands() {
        let mut config = Config::default();
        assert!(!config.tools.web.public_only && config.tools.gdb.allow_host_commands);
        restrict_remote_session(&mut config);
        assert!(config.tools.web.public_only);
        assert!(!config.tools.gdb.allow_host_commands);
    }

    /// sven holds no remotely driven session's tools to a directory of its
    /// own making: a person's computer and a container are the person's,
    /// and a hosted host is held by the kernel to its person's sandbox
    /// (Landlock), whatever tools it offers. A check in sven would only be a
    /// policy in the agent's way, as the shell beside it reaches everything.
    #[test]
    fn sven_confines_no_session_s_tools_itself() {
        let dir = tempfile::tempdir().unwrap();
        for (hosted, shared) in [(false, false), (true, false), (true, true)] {
            let scope = session_runtime(&Config::default(), dir.path(), hosted, shared)
                .unwrap()
                .path_scope;
            for path in [
                "/etc/hosts",
                "../elsewhere/notes.md",
                ".git/hooks/pre-commit",
                ".sven/config.yaml",
                ".config/sven/config.yaml",
            ] {
                assert!(
                    scope.resolve(path).is_ok(),
                    "{path} (hosted {hosted}, shared {shared})"
                );
            }
        }
    }

    /// The agent's own place for temporary files, instead of the shared
    /// `/tmp`: one per conversation, private to the person, kept while the
    /// conversation is (a restarted host or resumed pod finds it again),
    /// removed with it.
    #[test]
    fn each_conversation_has_a_private_scratch_directory_that_goes_with_it() {
        let cache = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let scratch_a = scratch_dir(cache.path(), a).unwrap();
        assert!(scratch_a.is_dir());
        assert!(scratch_a.starts_with(cache.path().join("sven").join("scratch")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&scratch_a).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        std::fs::write(scratch_a.join("clone.txt"), "x").unwrap();
        assert_eq!(scratch_dir(cache.path(), a).unwrap(), scratch_a);
        assert!(scratch_a.join("clone.txt").exists(), "a restart keeps it");

        let scratch_b = scratch_dir(cache.path(), b).unwrap();
        forget_scratch(cache.path(), b);
        assert!(!scratch_b.exists() && scratch_a.exists());
        forget_scratch(cache.path(), b);
    }

    #[test]
    fn each_hosted_conversation_has_a_directory_of_its_own_that_goes_with_it() {
        let work = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let dir_a = conversation_dir(work.path(), true, a).unwrap();
        let dir_b = conversation_dir(work.path(), true, b).unwrap();
        assert!(dir_a.is_dir() && dir_b.is_dir() && dir_a != dir_b);
        assert!(dir_a.starts_with(work.path()) && dir_b.starts_with(work.path()));
        // Its relative paths start there.
        let scope_a = session_runtime(&Config::default(), &dir_a, true, true)
            .unwrap()
            .path_scope;
        assert_eq!(
            scope_a.resolve("invoice.pdf").unwrap(),
            dir_a.canonicalize().unwrap().join("invoice.pdf")
        );
        // On a person's computer the conversation works in the directory
        // `sven rc` runs in.
        assert_eq!(
            conversation_dir(work.path(), false, a).unwrap(),
            work.path()
        );
        // A closed hosted conversation's files go with it; nothing else does.
        forget_conversation_files(work.path(), true, b);
        assert!(!dir_b.exists() && dir_a.exists());
        forget_conversation_files(work.path(), false, a);
        assert!(dir_a.exists(), "a person's own directory is never removed");
    }

    #[test]
    fn a_hosted_host_reads_documents_only_in_confined_workers_and_runs_nothing() {
        let mut config: Config = serde_yaml::from_str("tools: {}\n").unwrap();
        restrict_to_hosted(&mut config);
        for tool in ["document_convert", "document_locate", "run_model"] {
            assert!(config.tools.offers(tool), "{tool}");
        }
        for tool in [
            "shell",
            "read_file",
            "write_file",
            "edit_file",
            "grep",
            "gdb",
            "attach_file",
        ] {
            assert!(!config.tools.offers(tool), "{tool} must not be offered");
        }
        assert_eq!(config.ocr.isolate, Some(true));
        // Whatever the configuration says.
        let mut lax: Config = serde_yaml::from_str("ocr: {isolate: false}\n").unwrap();
        restrict_to_hosted(&mut lax);
        assert_eq!(lax.ocr.isolate, Some(true));
    }
}
