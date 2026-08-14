// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use crate::cli::McpCommands;

pub(crate) async fn run_mcp_command(cmd: &McpCommands) -> anyhow::Result<()> {
    match cmd {
        McpCommands::Serve {
            tools,
            brave_api_key,
            node_url,
            token,
            node_ca,
            insecure_tls,
        } => {
            if let Some(url) = node_url {
                let tok = token.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "--token (or SVEN_NODE_TOKEN) is required when --node-url is set"
                    )
                })?;
                let options = sven_node_client::ConnectOptions {
                    extra_ca_pem: node_ca.clone(),
                    insecure_dev: *insecure_tls,
                };
                sven_mcp::serve_stdio_node_proxy_with_options(url.clone(), tok, options).await
            } else {
                let registry = std::sync::Arc::new(sven_mcp::build_mcp_registry(
                    brave_api_key.clone(),
                    tools.as_deref(),
                ));
                sven_mcp::serve_stdio(registry).await
            }
        }
    }
}
