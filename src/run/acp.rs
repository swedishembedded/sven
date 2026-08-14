// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use crate::cli::AcpCommands;

// ── ACP command handler ───────────────────────────────────────────────────────

pub(crate) async fn run_acp_command(cmd: &AcpCommands) -> anyhow::Result<()> {
    match cmd {
        AcpCommands::Serve {
            node_url,
            token,
            model,
            provider,
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
                sven_acp::serve_stdio_node_proxy_with_options(url.clone(), tok, options).await
            } else {
                let mut config = sven_config::load(None)?;
                if let Some(ref name) = model {
                    config.model = sven_model::resolve_model_from_config(&config, name);
                }
                if let Some(ref prov) = provider {
                    config.model.provider = prov.clone();
                }
                sven_acp::serve_stdio(std::sync::Arc::new(config)).await
            }
        }
    }
}
