// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for giving agents one clear set of
// tools however many places a service is connected from, for its clients. If
// your team needs expertise in agent tool integration then you can procure
// our services by sending an email to info@swedishembedded.com.
//! One service connected twice: locally configured and through the
//! signed-in account. The local one wins, so the model never sees the same
//! tools twice.
//!
//! The account's server marks each tool it offers with `_meta`
//! `"com.swedishembedded/connector": {"name": ..., "url": ...}`, naming the
//! connector the tool belongs to and that connector's MCP URL.

use std::collections::HashMap;

use rmcp::model::MetaObject;

use crate::config::{McpServerConfig, McpTransport};

/// The `_meta` key under which a tool names its connector.
pub const CONNECTOR_META_KEY: &str = "com.swedishembedded/connector";

/// `url` reduced to what identifies a server: scheme, host and path in
/// lower case where case does not matter, without a trailing slash, query
/// or fragment.
fn normalized(url: &str) -> String {
    let url = url.trim().split(['?', '#']).next().unwrap_or("");
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.trim_end_matches('/').to_string();
    };
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    format!(
        "{}://{}/{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase(),
        path.trim_end_matches('/')
    )
    .trim_end_matches('/')
    .to_string()
}

/// Whether a tool of server `listed`, carrying `meta`, is a copy of a tool
/// of a locally configured server: its connector is called like an enabled
/// local server, or lives at an enabled local HTTP server's URL. `listed`
/// itself is never "local" to its own tools.
#[must_use]
pub fn duplicates_local(
    meta: Option<&MetaObject>,
    listed: &str,
    configured: &HashMap<String, McpServerConfig>,
) -> bool {
    let Some(connector) = meta.and_then(|m| m.0.get(CONNECTOR_META_KEY)) else {
        return false;
    };
    let name = connector.get("name").and_then(serde_json::Value::as_str);
    let url = connector
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(normalized);
    configured
        .iter()
        .filter(|(local, cfg)| local.as_str() != listed && cfg.enabled)
        .any(|(local, cfg)| {
            name == Some(local.as_str())
                || matches!(
                    (&cfg.transport, &url),
                    (McpTransport::Http { url: mine, .. }, Some(theirs)) if normalized(mine) == *theirs
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(url: &str) -> McpServerConfig {
        McpServerConfig {
            transport: McpTransport::Http {
                url: url.to_string(),
                headers: HashMap::new(),
            },
            ..McpServerConfig::default()
        }
    }

    fn meta(name: &str, url: &str) -> MetaObject {
        MetaObject(
            serde_json::json!({ CONNECTOR_META_KEY: {"name": name, "url": url} })
                .as_object()
                .unwrap()
                .clone(),
        )
    }

    /// A local `github` server and an account with `github` and `notion`.
    #[test]
    fn the_local_server_wins_over_the_accounts_copy_of_it() {
        let configured = HashMap::from([
            ("github".to_string(), http("https://api.githubcopilot.com/mcp/")),
            ("connectors".to_string(), http("https://sven.example/api/mcp")),
        ]);
        let github = meta("github", "https://api.githubcopilot.com/mcp");
        let notion = meta("notion", "https://mcp.notion.com/mcp");
        assert!(duplicates_local(Some(&github), "connectors", &configured));
        assert!(!duplicates_local(Some(&notion), "connectors", &configured));

        // By URL alone, whatever the connector is called and however the
        // address is written.
        let renamed = meta("gh", "HTTPS://API.githubcopilot.com/mcp/?x=1");
        assert!(duplicates_local(Some(&renamed), "connectors", &configured));
        // By name alone (a stdio server has no URL).
        let by_name = HashMap::from([(
            "notion".to_string(),
            McpServerConfig {
                transport: McpTransport::Stdio {
                    command: "npx".into(),
                    args: vec![],
                },
                ..McpServerConfig::default()
            },
        )]);
        assert!(duplicates_local(Some(&notion), "connectors", &by_name));
    }

    #[test]
    fn a_tool_without_a_connector_or_for_a_disabled_local_server_stays() {
        let mut off = http("https://api.githubcopilot.com/mcp");
        off.enabled = false;
        let configured = HashMap::from([("github".to_string(), off)]);
        let github = meta("github", "https://api.githubcopilot.com/mcp");
        assert!(!duplicates_local(Some(&github), "connectors", &configured));
        assert!(!duplicates_local(None, "connectors", &configured));
        // A server is never a duplicate of itself.
        let configured = HashMap::from([("github".to_string(), http("https://a.example/mcp"))]);
        assert!(!duplicates_local(Some(&github), "github", &configured));
    }
}
