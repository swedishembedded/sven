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
/// of them reads, writes or runs anything on the machine the host runs on.
pub const HOSTED_TOOLS: &[&str] = &["web_search", "web_fetch", "skill"];

/// The system prompt of a hosted conversation when the site configures none
/// (`agent.system_prompt` in the file `sven --config` reads).
pub const HOSTED_SYSTEM_PROMPT: &str = "You are sven, a helpful assistant. Answer clearly and \
     concisely, and say so when you are not sure. You can search the web and read public web \
     pages; say which sources you used. You have no access to the person's computer or files.";

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
) -> anyhow::Result<RuntimeContext> {
    let mut runtime = RuntimeContext::auto_detect_at(Some(working_dir.to_path_buf()));
    runtime.project_root = Some(working_dir.to_path_buf());
    if !hosted && !wide_access {
        runtime.path_scope = confined_scope(working_dir, &protected_locations())?;
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

/// What no tool of a remotely driven session may read or write, even when it
/// lies inside the working directory (a session started in the home
/// directory): the sign-in, the stored provider keys and sven's own
/// configuration (all in `<config dir>/sven`), and the sessions this host
/// keeps for other conversations.
fn protected_locations() -> Vec<PathBuf> {
    let mut locations = Vec::new();
    locations.extend(dirs::config_dir().map(|dir| dir.join("sven")));
    locations.extend(crate::sessions::default_dir());
    locations
}

fn confined_scope(working_dir: &Path, protected: &[PathBuf]) -> anyhow::Result<PathScope> {
    let scope = PathScope::confined(working_dir)?;
    Ok(protected
        .iter()
        .fold(scope, |scope, location| scope.denying(location)))
}

/// What a remotely driven session on a person's computer may reach unless
/// the person asks for more (`sven rc --wide-access`). The server chooses
/// the tool calls and relays the approvals, so neither is a barrier: the
/// file tools stay in the working directory (`session_runtime`), `web_fetch`
/// reaches only the public internet (never this machine's network or a cloud
/// metadata service), and gdb runs no host program the model spells out.
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
/// metadata service).
pub fn restrict_to_hosted(config: &mut Config) {
    config.tools.only = Some(
        HOSTED_TOOLS
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
    );
    config.tools.web.public_only = true;
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn only_a_person_computer_session_without_wide_access_is_confined() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::default();
        let root = |hosted, wide| {
            session_runtime(&config, dir.path(), hosted, wide)
                .unwrap()
                .path_scope
                .root()
                .map(Path::to_path_buf)
        };
        assert_eq!(root(false, false), Some(dir.path().canonicalize().unwrap()));
        assert_eq!(root(false, true), None);
        // A hosted host is held by the kernel and its tool list instead.
        assert_eq!(root(true, false), None);
    }
}
