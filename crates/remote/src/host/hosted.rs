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
/// On a person's computer the file tools are also confined to `working_dir`
/// and kept out of the host's own credentials, unless `wide_access`
/// ([`restrict_to_working_dir`]).
///
/// # Errors
///
/// The working directory cannot be used as the root of a confined scope.
pub(super) fn session_runtime(
    config: &Config,
    working_dir: &Path,
    hosted: bool,
    wide_access: bool,
    shared_workdir: bool,
) -> anyhow::Result<RuntimeContext> {
    let mut runtime = RuntimeContext::auto_detect_at(Some(working_dir.to_path_buf()));
    runtime.project_root = Some(working_dir.to_path_buf());
    // A hosted host in a sandbox is held by the kernel to its person's
    // environment, which holds every conversation of theirs; its sessions'
    // path-taking tools are held to their own conversation's directory as
    // well. A container serving one conversation is its own boundary.
    if shared_workdir || (!hosted && !wide_access) {
        runtime.path_scope = conversation_files(config, working_dir).into_iter().fold(
            confined_scope(working_dir, &protected_locations())?,
            PathScope::allowing_beneath,
        );
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

/// What no tool of a remotely driven session may read or write, even when it
/// lies inside the working directory (a session started in the home
/// directory): the sign-in, the stored provider keys and sven's own
/// configuration (`<config dir>/sven`, and `~/.config/sven`, read on every
/// platform), and everything sven keeps in `<data dir>/sven`: the sessions
/// this host keeps for other conversations, and the person's own chats and
/// traces, which hold whatever they ever pasted into sven.
fn protected_locations() -> Vec<PathBuf> {
    let mut locations = Vec::new();
    locations.extend(dirs::config_dir().map(|dir| dir.join("sven")));
    locations.extend(dirs::home_dir().map(|home| home.join(".config").join("sven")));
    locations.extend(dirs::data_dir().map(|dir| dir.join("sven")));
    locations
}

/// What no tool of a remotely driven session may write at any depth of the
/// working directory, because a program the person runs there later executes
/// it with their own rights, outside every confinement: git's hooks and
/// configuration (`core.hooksPath`, `core.fsmonitor`), and sven's own
/// configuration of a directory, which can start MCP servers.
fn protected_names() -> impl Iterator<Item = &'static str> {
    let config = sven_config::scope::DIRECTORY_FILE_NAMES
        .iter()
        .filter_map(|name| name.split('/').next());
    std::iter::once(".git").chain(config)
}

fn confined_scope(working_dir: &Path, protected: &[PathBuf]) -> anyhow::Result<PathScope> {
    let scope = PathScope::confined(working_dir)?.denying_names(protected_names());
    Ok(protected
        .iter()
        .fold(scope, |scope, location| scope.denying(location)))
}

/// The directories sven itself fills with the conversation's files, below
/// its denied configuration directory: the person's attachments, files
/// fetched by handle, and what models made. A session's tools reach them;
/// nothing else of `.sven` (and no denied name below them).
fn conversation_files(config: &Config, working_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        working_dir.join(super::attachments::DIR),
        working_dir.join(sven_tools_media::FETCHED_DIR),
    ];
    let media = &config.tools.media.dir;
    if media.is_relative() {
        dirs.push(working_dir.join(media));
    }
    dirs
}

/// What a remotely driven session on a person's computer may reach unless
/// the person asks for more (`sven rc --wide-access`). The server chooses
/// the tool calls and relays the approvals, so neither is a barrier: the
/// file tools stay in the working directory (`session_runtime`), `web_fetch`
/// reaches only the public internet (never this machine's network or a cloud
/// metadata service), and gdb runs no host program the model spells out.
/// Nor can the file tools leave behind what runs later outside the directory
/// (git hooks, a sven configuration: `protected_names`).
/// The `shell` tool is not confined to the directory; `tools.only` and
/// `tools.disabled` remove it.
pub fn restrict_to_working_dir(config: &mut Config) {
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

    /// A file tool on a person's computer cannot leave behind what runs later
    /// outside the confinement with the person's own rights: git's hooks and
    /// configuration, or a sven configuration (which can start MCP servers)
    /// that sven reads when it next runs in that directory or one below it.
    #[test]
    fn a_confined_session_cannot_write_what_runs_later_outside_it() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = session_runtime(&Config::default(), tmp.path(), false, false, false).unwrap();
        for path in [
            ".git/hooks/pre-commit",
            ".git/config",
            "vendor/lib/.git/hooks/post-checkout",
            ".sven/config.yaml",
            ".sven.yaml",
            ".sven.yml",
            "sub/sven.yaml",
            "sven.yml",
        ] {
            assert!(runtime.path_scope.resolve(path).is_err(), "{path}");
        }
        assert!(runtime.path_scope.resolve("src/main.rs").is_ok());
    }

    /// The person's own chats and traces, and this host's kept sessions, are
    /// all under sven's data directory, which is protected as a whole.
    #[test]
    fn what_sven_keeps_about_the_person_is_protected() {
        let protected = protected_locations();
        let kept = [
            crate::sessions::default_dir().unwrap(),
            dirs::data_dir().unwrap().join("sven").join("chats"),
            dirs::home_dir().unwrap().join(".config/sven/remote.yaml"),
        ];
        for path in kept {
            assert!(
                protected.iter().any(|p| path.starts_with(p)),
                "{}",
                path.display()
            );
        }
    }

    #[test]
    fn a_remotely_driven_session_gets_a_public_only_web_and_no_gdb_host_commands() {
        let mut config = Config::default();
        assert!(!config.tools.web.public_only && config.tools.gdb.allow_host_commands);
        restrict_to_working_dir(&mut config);
        assert!(config.tools.web.public_only);
        assert!(!config.tools.gdb.allow_host_commands);
    }

    #[test]
    fn file_tools_stay_in_the_working_directory_and_out_of_the_credentials() {
        let home = tempfile::tempdir().unwrap();
        let work = home.path().join("project");
        let config_dir = home.path().join(".config/sven");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("remote.yaml"), "token: t").unwrap();

        let scope = confined_scope(&work, std::slice::from_ref(&config_dir)).unwrap();
        assert!(scope.resolve("src/main.rs").is_ok());
        for outside in [
            "../.config/sven/remote.yaml",
            "/etc/passwd",
            "../.ssh/id_ed25519",
        ] {
            assert!(scope.resolve(outside).is_err(), "{outside}");
        }

        // Started in the home directory, the credentials are inside the
        // working directory and still refused.
        let scope = confined_scope(home.path(), &[config_dir]).unwrap();
        assert!(scope.resolve("project/notes.md").is_ok());
        assert!(scope.resolve(".config/sven/remote.yaml").is_err());
    }

    #[test]
    fn each_hosted_conversation_has_a_directory_of_its_own_that_goes_with_it() {
        let work = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let dir_a = conversation_dir(work.path(), true, a).unwrap();
        let dir_b = conversation_dir(work.path(), true, b).unwrap();
        assert!(dir_a.is_dir() && dir_b.is_dir() && dir_a != dir_b);
        assert!(dir_a.starts_with(work.path()) && dir_b.starts_with(work.path()));
        // One conversation's tools cannot reach the other's files.
        std::fs::write(dir_b.join("invoice.pdf"), "x").unwrap();
        let scope_a = session_runtime(&Config::default(), &dir_a, true, false, true)
            .unwrap()
            .path_scope;
        assert!(scope_a.resolve("../").is_err());
        assert!(scope_a.resolve(dir_b.join("invoice.pdf")).is_err());
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

    /// The files sven keeps for the conversation below `.sven` are its
    /// tools' to read: the attachment the person's message names by path
    /// opens; sven's configuration beside it does not.
    #[test]
    fn a_session_reads_its_attachments_but_never_sven_s_configuration() {
        let work = tempfile::tempdir().unwrap();
        let scope = session_runtime(&Config::default(), work.path(), true, false, true)
            .unwrap()
            .path_scope;
        for path in [
            ".sven/attachments/f1/invoice.pdf",
            ".sven/files/7b1d.pdf",
            ".sven/media/page-1.png",
        ] {
            assert!(scope.resolve(path).is_ok(), "{path}");
        }
        for path in [".sven/config.yaml", ".sven/attachments/f1/.git/config"] {
            assert!(scope.resolve(path).is_err(), "{path}");
        }
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

    #[test]
    fn only_a_person_computer_session_without_wide_access_is_confined() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let root = |hosted, wide, shared| {
            session_runtime(&config, dir.path(), hosted, wide, shared)
                .unwrap()
                .path_scope
                .root()
                .map(Path::to_path_buf)
        };
        assert_eq!(
            root(false, false, false),
            Some(dir.path().canonicalize().unwrap())
        );
        assert_eq!(root(false, true, false), None);
        // A container serving one conversation is its own boundary: its
        // shell and file tools work across its workspace and the account's
        // shared files, as before.
        assert_eq!(root(true, false, false), None);
        // A hosted session, held by the kernel to its person's environment,
        // is also held to its own conversation's directory: documents of
        // the person's other conversations are not its to read, also for
        // someone the conversation is shared with.
        assert_eq!(
            root(true, false, true),
            Some(dir.path().canonicalize().unwrap())
        );
    }
}
