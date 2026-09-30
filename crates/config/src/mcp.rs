// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! MCP server configuration: how sven reaches each configured server.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::schema::default_true;

/// Transport configuration for an MCP server.
///
/// Supports stdio (subprocess) and HTTP (streamable HTTP / SSE) transports.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
/// Tokens are cached in `~/.config/sven/mcp-credentials.json` and refreshed
/// automatically before expiry.
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
}

fn default_mcp_timeout() -> u64 {
    30
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
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            transport: McpTransport::default(),
            enabled: true,
            env: HashMap::new(),
            oauth: None,
            timeout_secs: default_mcp_timeout(),
        }
    }
}
