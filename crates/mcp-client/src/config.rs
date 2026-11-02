// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `mcp_servers:` section of the configuration file: how sven reaches
//! each configured MCP server.
//!
//! Owned here because this crate is what reads it: [`crate::McpManager`] is
//! built from a [`McpServers`] map. The section's keys are declared beside the
//! types ([`mcp_servers_schema`]).
//!
//! Swedish Embedded AB implements MCP client integration for its clients. If
//! your team needs expertise in connecting agents to Model Context Protocol
//! servers then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sven_config::{default_true, Schema};

/// The configured servers, by the name that prefixes their tools.
pub type McpServers = HashMap<String, McpServerConfig>;

/// The keys of the `mcp_servers:` section: an entry per server, named by the
/// user.
#[must_use]
pub fn mcp_servers_schema() -> Schema {
    Schema::entries(McpServerConfig::schema())
}

/// Transport configuration for an MCP server.
///
/// Supports stdio (subprocess) and HTTP (streamable HTTP / SSE) transports.
/// Its `Debug` names an HTTP server's headers but never their values: they
/// carry keys.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpTransport {
    /// Run an MCP server as a child process communicating over stdin/stdout.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Connect to an HTTP-based MCP server (Streamable HTTP or SSE).
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

impl std::fmt::Debug for McpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpTransport::Stdio { command, args } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .finish(),
            McpTransport::Http { url, headers } => {
                let mut names: Vec<&String> = headers.keys().collect();
                names.sort();
                f.debug_struct("Http")
                    .field("url", url)
                    .field("headers", &names)
                    .finish_non_exhaustive()
            }
        }
    }
}

impl Default for McpTransport {
    fn default() -> Self {
        McpTransport::Stdio {
            command: String::new(),
            args: vec![],
        }
    }
}

/// OAuth configuration for an MCP server that requires OAuth 2.0 authentication.
///
/// When present, sven will use the OAuth PKCE flow (RFC 7636) to obtain tokens.
/// Tokens are kept in `~/.config/sven/mcp-oauth.json` and refreshed
/// automatically when they expire.
///
/// **Scopes are optional** - sven follows the MCP Authorization spec scope
/// discovery strategy and discovers them automatically:
///
/// 1. `scope` parameter in the `WWW-Authenticate` header of the 401 response.
/// 2. `scopes_supported` from the server's Protected Resource Metadata
///    (`/.well-known/oauth-protected-resource`, RFC 9728).
/// 3. Omit the `scope` parameter if neither source is available.
///
/// You only need to set `scopes` if you want to override the discovered values.
///
/// Minimal config that works for most OAuth-protected MCP servers:
/// ```yaml
/// mcp_servers:
///   atlassian:
///     transport:
///       type: http
///       url: "https://mcp.atlassian.com/v2"
///     oauth: {}
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpOAuthConfig {
    /// OAuth scopes to request.  Leave empty to have sven discover them
    /// automatically from the server (recommended).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Pre-registered OAuth client ID.  Leave absent to use the default
    /// `sven-mcp-client` public client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Pre-registered OAuth client secret (only needed for confidential clients).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Custom redirect URI for OAuth. When absent, sven uses `sven://sven.mcp/callback`
    /// (installed by the Debian package) when not in a container, or
    /// `http://127.0.0.1:5598/callback` when running in a container (Docker, etc.).
    /// Use `cursor://cursor.mcp/callback` for Atlassian MCP (pre-allowlisted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
    /// Port for the local callback server when using a custom redirect_uri.
    /// The protocol handler must forward to `http://127.0.0.1:{port}/callback`.
    /// Default: 5598. When in a container, ensure this port is forwarded (e.g. -p 5598:5598).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
    /// HTTPS URL of a Client ID Metadata Document describing this client.
    /// When the authorization server supports such documents it is used as
    /// the client id instead of registering one (Dynamic Client
    /// Registration).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_metadata_url: Option<String>,
}

fn default_mcp_timeout() -> u64 {
    30
}

/// What sven may do with one of a server's tools without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpToolPolicy {
    /// Runs without asking, in every approval mode.
    Allow,
    /// Asks a person first, unless the session's approval mode runs tools
    /// without asking.
    Ask,
    /// Never offered, never run.
    Deny,
}

/// The policy of each of a server's tools: by its name, `*` for every tool
/// not named; a tool in neither asks. A server's own hints only ever
/// tighten it: a tool the server marks destructive asks under a `*: allow`
/// unless it is named itself.
#[must_use]
pub fn tool_policy(
    policies: &HashMap<String, McpToolPolicy>,
    tool: &str,
    destructive: bool,
) -> McpToolPolicy {
    if let Some(named) = policies.get(tool) {
        return *named;
    }
    match policies.get("*") {
        Some(McpToolPolicy::Allow) if destructive => McpToolPolicy::Ask,
        Some(policy) => *policy,
        None => McpToolPolicy::Ask,
    }
}

/// Configuration for a single external MCP server.
///
/// MCP servers extend sven with additional tools, prompts, and resources.
/// Each server is identified by its key in the `mcp_servers` map; that key
/// is used as the tool prefix (e.g. server `"github"` → tool `"github-list_repos"`).
///
/// Example YAML:
/// ```yaml
/// mcp_servers:
///   github:
///     transport:
///       type: stdio
///       command: npx
///       args: ["-y", "@modelcontextprotocol/server-github"]
///     env:
///       GITHUB_TOKEN: "${GITHUB_TOKEN}"
///     enabled: true
///
///   atlassian:
///     transport:
///       type: http
///       url: "https://mcp.atlassian.com/v2"
///     oauth: {}   # scopes auto-discovered from the server
///     enabled: true
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    /// Transport to use for this MCP server.
    pub transport: McpTransport,
    /// Whether this MCP server is active.  Disabled servers are not connected
    /// and their tools are not available to the agent.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Environment variables to pass to a stdio MCP server subprocess.
    /// Values support `${VAR}` and `${VAR:-default}` expansion.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// OAuth configuration.  Required for HTTP servers that use OAuth 2.0.
    /// For servers that use a static bearer token, set it in `headers` instead.
    #[serde(default)]
    pub oauth: Option<McpOAuthConfig>,
    /// Request timeout in seconds.
    #[serde(default = "default_mcp_timeout")]
    pub timeout_secs: u64,
    /// What sven may do with each tool without asking ([`tool_policy`]).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tools: HashMap<String, McpToolPolicy>,
    /// For an HTTP server reached with short-lived tokens traded for a key
    /// ([`crate::exchange`]). Set by sven itself, never read from or written
    /// to a configuration file: it carries the key.
    #[serde(skip)]
    pub token_exchange: Option<crate::TokenExchange>,
}

impl McpServerConfig {
    /// A Streamable HTTP server at `url` that takes `token` as a bearer
    /// token.
    #[must_use]
    pub fn bearer_http(url: &str, token: &str) -> Self {
        Self {
            transport: McpTransport::Http {
                url: url.to_string(),
                headers: HashMap::from([("Authorization".to_string(), format!("Bearer {token}"))]),
            },
            ..Self::default()
        }
    }

    /// A Streamable HTTP server at `url` reached with tokens traded for
    /// `key` at `token_url` ([`crate::exchange`]).
    #[must_use]
    pub fn exchanged_http(url: &str, token_url: &str, key: &str) -> Self {
        Self {
            transport: McpTransport::Http {
                url: url.to_string(),
                headers: HashMap::new(),
            },
            token_exchange: Some(crate::TokenExchange {
                url: token_url.to_string(),
                key: key.to_string(),
            }),
            ..Self::default()
        }
    }

    /// The keys of an `mcp_servers.<name>` entry.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::fields([
            // stdio: type, command, args; http: type, url, headers.
            (
                "transport",
                Schema::keys(&["type", "command", "args", "url", "headers"]),
            ),
            ("enabled", Schema::value()),
            ("env", Schema::value()),
            (
                "oauth",
                Schema::keys(&[
                    "scopes",
                    "client_id",
                    "client_secret",
                    "redirect_uri",
                    "callback_port",
                    "client_metadata_url",
                ]),
            ),
            ("timeout_secs", Schema::value()),
            ("tools", Schema::value()),
        ])
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            transport: McpTransport::default(),
            enabled: true,
            env: HashMap::new(),
            oauth: None,
            timeout_secs: default_mcp_timeout(),
            tools: HashMap::new(),
            token_exchange: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_servers_headers_are_named_but_never_shown() {
        let server = McpServerConfig::bearer_http("https://mcp.example/mcp", "the-secret-key");
        let shown = format!("{:?}", server.transport);
        assert!(shown.contains("Authorization"), "{shown}");
        assert!(!shown.contains("the-secret-key"), "{shown}");
    }

    #[test]
    fn a_server_defaults_to_enabled_with_a_thirty_second_timeout() {
        let server: McpServerConfig =
            serde_yaml::from_str("transport: {type: http, url: 'https://x'}\n").unwrap();
        assert!(server.enabled);
        assert_eq!(server.timeout_secs, 30);
        assert!(server.env.is_empty() && server.oauth.is_none());
    }

    #[test]
    fn a_tool_policy_is_by_name_then_star_and_a_destructive_hint_only_tightens() {
        let server: McpServerConfig = serde_yaml::from_str(
            "transport: {type: http, url: 'https://x'}\ntools: {'*': allow, delete_repo: allow, push: deny}\n",
        )
        .unwrap();
        let p = |tool, destructive| tool_policy(&server.tools, tool, destructive);
        assert_eq!(p("list_issues", false), McpToolPolicy::Allow);
        assert_eq!(p("close_issue", true), McpToolPolicy::Ask);
        assert_eq!(p("delete_repo", true), McpToolPolicy::Allow);
        assert_eq!(p("push", false), McpToolPolicy::Deny);
        assert_eq!(
            tool_policy(&HashMap::new(), "anything", false),
            McpToolPolicy::Ask
        );
    }

    #[test]
    fn the_transport_is_chosen_by_its_type() {
        let stdio: McpTransport =
            serde_yaml::from_str("type: stdio\ncommand: npx\nargs: [a, b]\n").unwrap();
        assert!(
            matches!(stdio, McpTransport::Stdio { command, args } if command == "npx" && args == ["a", "b"])
        );
        let http: McpTransport = serde_yaml::from_str("type: http\nurl: u\n").unwrap();
        assert!(
            matches!(http, McpTransport::Http { url, headers } if url == "u" && headers.is_empty())
        );
    }
}
