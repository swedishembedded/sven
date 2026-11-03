// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What the server a host signed in to provides the host's sessions: its web
//! search and the person's connected accounts.

use sven_bootstrap::Config;

use crate::login::RemoteLogin;

/// What the server a host signed in to provides its sessions, with the
/// host's key: web searches (run and billed there), and the person's
/// connected accounts as the `connectors` MCP server when the server offers
/// them. A hosted host's allow list admits that one server's tools; it
/// starts no other MCP server (`restrict_to_hosted`).
pub(super) fn use_server_services(config: &mut Config, login: &RemoteLogin) {
    config.tools.web.search.sven = Some(login.search_endpoint());
    if let Some(url) = &login.mcp_url {
        sven_bootstrap::connectors::add(config, url, login.mcp_token_url.as_deref(), &login.token);
        if let Some(only) = config.tools.only.as_mut() {
            only.push(format!("{}-*", sven_bootstrap::connectors::SERVER));
            // Deferral of connector tools needs the discovery tool.
            only.push(sven_bootstrap::DISCOVERY_TOOL.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    /// A hosted host reaches the person's connected accounts at its server
    /// and nothing else: the `connectors` server is added to an emptied
    /// server list, and its tools alone join the allow list.
    #[test]
    fn a_hosted_host_reaches_connected_accounts_at_its_server_alone() {
        let mut config: Config = serde_yaml::from_str(
            "mcp_servers:\n  local: {transport: {type: stdio, command: /bin/true}}\n",
        )
        .unwrap();
        crate::restrict_to_hosted(&mut config);
        let login = RemoteLogin {
            server: "https://sven.example".into(),
            control_url: "https://sven.example/api/workspace".into(),
            openai_base_url: "https://sven.example/v1".into(),
            mcp_url: Some("https://sven.example/api/mcp".into()),
            mcp_token_url: Some("https://sven.example/api/mcp/token".into()),
            token: "host_key".into(),
            workspace_id: Uuid::nil(),
            workspace_name: "Hosted".into(),
            account: "a@example.com".into(),
        };
        use_server_services(&mut config, &login);
        assert_eq!(
            config.mcp_servers.keys().collect::<Vec<_>>(),
            vec![sven_bootstrap::connectors::SERVER]
        );
        let only = config.tools.only.as_ref().unwrap();
        assert!(only.iter().any(|t| t == "connectors-*"));
        assert!(only.iter().any(|t| t == "web_search"));
        assert!(
            only.iter().any(|t| t == sven_bootstrap::DISCOVERY_TOOL),
            "without the discovery tool every connector tool is sent in full on every request"
        );

        let mut without: Config = Config::default();
        crate::restrict_to_hosted(&mut without);
        let hosted = without.tools.only.clone();
        let login = RemoteLogin {
            mcp_url: None,
            mcp_token_url: None,
            ..login
        };
        use_server_services(&mut without, &login);
        assert_eq!(
            without.tools.only, hosted,
            "no connectors, no change to the hosted allow list"
        );
    }
}
