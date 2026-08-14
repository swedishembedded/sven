// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::Subcommand;

// ── Mcp subcommand ────────────────────────────────────────────────────────────

/// `sven mcp` subcommands.
#[derive(Subcommand, Debug)]
pub enum McpCommands {
    /// Expose sven as an MCP server over stdio.
    ///
    /// Starts a Model Context Protocol server that speaks line-delimited
    /// JSON-RPC on stdin/stdout.  Any MCP-compatible host can launch sven
    /// as a subprocess and call its tools:
    ///
    ///   Cursor / Claude Desktop / opencode (`mcp.json`):
    ///
    ///   { "mcpServers": { "sven": { "command": "sven", "args": ["mcp", "serve"] } } }
    ///
    /// The server blocks until stdin reaches EOF (i.e. until the host
    /// disconnects).  It does not fork, does not bind a port, and requires
    /// no authentication - security is inherited from the host process.
    Serve {
        /// Comma-separated list of tool names to expose (local mode only).
        ///
        /// Defaults to all MCP-safe built-in tools (see `sven_mcp::DEFAULT_TOOL_NAMES`).
        /// Pass `all` to include every registered tool explicitly.
        /// Ignored when `--node-url` is set (the node's own registry is used).
        ///
        /// Example: --tools read_file,write_file,grep,run_terminal_command
        #[arg(long, value_name = "TOOL,...")]
        tools: Option<String>,

        /// Brave Search API key for the web_search tool (local mode only).
        ///
        /// May also be provided via the BRAVE_API_KEY environment variable.
        /// Ignored when `--node-url` is set.
        #[arg(long, env = "BRAVE_API_KEY", value_name = "KEY")]
        brave_api_key: Option<String>,

        /// WebSocket URL of a running `sven node` to proxy tool calls through.
        ///
        /// When provided, the MCP server connects to the node over WebSocket
        /// and forwards every tool call to it.  This exposes the full node tool
        /// registry, including P2P tools like `list_peers` and `delegate_task`.
        ///
        /// Example: --node-url wss://127.0.0.1:18790/ws
        #[arg(long, value_name = "URL")]
        node_url: Option<String>,

        /// Bearer token for authenticating with the sven node.
        ///
        /// Required when `--node-url` is set.  This is the raw token printed by
        /// `sven node start` on first launch (not the hash stored on disk).
        ///
        /// May also be provided via the SVEN_NODE_TOKEN environment variable.
        /// The legacy name SVEN_GATEWAY_TOKEN is also accepted.
        #[arg(long, env = "SVEN_NODE_TOKEN", value_name = "TOKEN")]
        token: Option<String>,

        /// Extra PEM CA certificate file to trust when verifying the node's
        /// TLS certificate (e.g. a remote node's `ca-cert.pem`).
        ///
        /// The system trust store and the local node CA
        /// (`~/.config/sven/node/tls/ca-cert.pem`) are always trusted.
        #[arg(long, value_name = "FILE")]
        node_ca: Option<std::path::PathBuf>,

        /// DANGER: skip TLS certificate verification when connecting to the
        /// node. Local testing against a `self-signed` node only - never use
        /// this in production.
        #[arg(long)]
        insecure_tls: bool,
    },
}

