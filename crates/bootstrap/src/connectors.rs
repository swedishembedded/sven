// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for giving agents the accounts
// their people connected, without handing them the sign-ins, for its clients.
// If your team needs expertise in agent tool integration then you can procure
// our services by sending an email to info@swedishembedded.com.
//! The `connectors` MCP server: the accounts a person connected on the
//! server sven is signed in to (`sven auth login`, `sven rc`), reached at the
//! MCP endpoint that server advertised, with the same key. To sven it is an
//! ordinary remote MCP server; the accounts' own sign-ins stay on the server.
//!
//! Added by the run modes that start sessions, never written to a file (it
//! carries the key). A configured `mcp_servers.connectors` entry takes its
//! place: `connectors: {enabled: false}` turns it off.

use crate::Config;

/// The server's name, which prefixes its tools (`connectors-<tool>`).
pub const SERVER: &str = "connectors";

/// Add the `connectors` server at `mcp_url` with `token`, unless the
/// configuration names one of its own.
pub fn add(config: &mut Config, mcp_url: &str, token: &str) {
    config
        .mcp_servers
        .entry(SERVER.to_string())
        .or_insert_with(|| sven_mcp_client::McpServerConfig::bearer_http(mcp_url, token));
}

/// [`add`] for the server this user signed in to, when it advertised an MCP
/// endpoint.
pub fn add_signed_in(config: &mut Config) {
    if let Some(signed_in) = sven_model_drivers::sven_service::signed_in() {
        if let Some(url) = &signed_in.mcp_url {
            add(config, url, &signed_in.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_is_added_unless_configured() {
        let mut config = Config::default();
        add(&mut config, "https://sven.example/api/mcp", "key");
        let server = &config.mcp_servers[SERVER];
        assert!(matches!(
            &server.transport,
            sven_mcp_client::McpTransport::Http { url, headers }
                if url == "https://sven.example/api/mcp" && headers["Authorization"] == "Bearer key"
        ));

        let mut config: Config =
            serde_yaml::from_str("mcp_servers:\n  connectors: {enabled: false}\n").unwrap();
        add(&mut config, "https://sven.example/api/mcp", "key");
        assert!(!config.mcp_servers[SERVER].enabled);
    }
}
