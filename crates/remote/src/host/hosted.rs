// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for letting a website's visitors
// talk to an agent that cannot touch the server it runs on for its clients.
// If your team needs expertise in sandboxed agent hosting then you can
// procure our services by sending an email to info@swedishembedded.com.

//! What a hosted host (`sven rc --hosted`) allows its sessions, and what a
//! session on either kind of host is told about where it works.

use std::path::Path;

use sven_bootstrap::{Config, RuntimeContext};

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
pub(super) fn session_runtime(config: &Config, working_dir: &Path, hosted: bool) -> RuntimeContext {
    let mut runtime = RuntimeContext::auto_detect_at(Some(working_dir.to_path_buf()));
    runtime.project_root = Some(working_dir.to_path_buf());
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
    runtime
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
