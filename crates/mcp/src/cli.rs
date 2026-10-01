// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `sven mcp` clap grammar and handler, reusable both by the monolithic
//! `sven` binary's `Commands::Mcp` dispatch and the standalone `sven-mcp`
//! binary.

use clap::Subcommand;

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
    ///
    /// Every served tool runs when the host calls it: the host applies its
    /// own permission UI. The file tools and the shell's working directory
    /// are confined to the directory the server starts in, and the shell
    /// refuses a command matching `tools.deny_patterns`.
    Serve {
        /// Comma-separated list of tool names to expose.
        ///
        /// Defaults to all MCP-safe built-in tools (see `sven_mcp::DEFAULT_TOOL_NAMES`).
        /// Pass `all` to include every registered tool explicitly.
        ///
        /// Example: --tools read_file,write_file,grep,run_terminal_command
        #[arg(long, value_name = "TOOL,...")]
        tools: Option<String>,

        /// Brave Search API key for the web_search tool.
        ///
        /// May also be provided via the BRAVE_API_KEY environment variable.
        #[arg(long, env = "BRAVE_API_KEY", value_name = "KEY")]
        brave_api_key: Option<String>,
    },
}

/// Runs a `sven mcp` subcommand.
pub async fn run_mcp_command(cmd: &McpCommands) -> anyhow::Result<()> {
    match cmd {
        McpCommands::Serve {
            tools,
            brave_api_key,
        } => {
            let config = sven_bootstrap::Config::load(None)?;
            let root = std::env::current_dir()?;
            let scope = sven_tool_api::PathScope::confined(&root)
                .map_err(|e| anyhow::anyhow!("serving {}: {e}", root.display()))?;
            let registry = std::sync::Arc::new(crate::build_mcp_registry(
                brave_api_key.clone(),
                tools.as_deref(),
                &config.tools,
                scope,
            ));
            crate::serve_stdio(registry).await
        }
    }
}
