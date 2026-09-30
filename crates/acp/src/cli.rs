// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `sven acp` clap grammar and handler, reusable both by the monolithic
//! `sven` binary's `Commands::Acp` dispatch and the standalone `sven-acp`
//! binary.

use clap::Subcommand;

/// `sven acp` subcommands.
#[derive(Subcommand, Debug)]
pub enum AcpCommands {
    /// Expose sven as a full ACP agent over stdio.
    ///
    /// Starts an Agent Client Protocol server that speaks JSON-RPC 2.0 on
    /// stdin/stdout.  Any ACP-compatible IDE (JetBrains, Zed, VS Code with an
    /// ACP extension) can launch sven as a subprocess and interact with it as
    /// a first-class AI coding agent with sessions, streaming, plans, and mode
    /// switching.
    ///
    ///   JetBrains / Zed / VS Code (`acp.json`):
    ///
    ///   { "agents": { "sven": { "command": "sven", "args": ["acp", "serve"] } } }
    ///
    /// The server blocks until stdin reaches EOF (i.e. until the IDE
    /// disconnects the subprocess).
    Serve {
        /// Override the model used by this ACP server instance.
        ///
        /// Accepts a bare model name ("claude-sonnet-4-6") or a
        /// "provider/model" pair ("anthropic/claude-sonnet-4-6").
        ///
        /// The task tool passes the parent agent's current model here so that
        /// sub-agents automatically inherit the same model.
        ///
        /// May also be set via the SVEN_MODEL environment variable.
        #[arg(long, env = "SVEN_MODEL", value_name = "MODEL")]
        model: Option<String>,

        /// Override only the provider without changing the model name.
        ///
        /// May also be set via the SVEN_PROVIDER environment variable.
        #[arg(long, env = "SVEN_PROVIDER", value_name = "PROVIDER")]
        provider: Option<String>,

        /// Lowers `agent.max_tool_rounds` to at most `N` tool rounds a turn.
        ///
        /// The task tool passes its sub-agent's budget here, so a sub-agent
        /// never takes more rounds than the session that started it.
        #[arg(long, value_name = "N")]
        max_tool_rounds: Option<u32>,

        /// Lowers the model's output-token cap to at most `TOKENS` a response.
        #[arg(long, value_name = "TOKENS")]
        max_output_tokens: Option<u32>,

        /// Stops serving, and every session with it, after `SECS` seconds.
        #[arg(long, value_name = "SECS")]
        wall_clock_secs: Option<u64>,

        /// Tools, by name, this server's sessions never offer nor run, on top
        /// of `tools.disabled`. Repeatable.
        #[arg(long = "disable-tool", value_name = "NAME")]
        disable_tools: Vec<String>,

        /// How long a tool call waits for the client's permission answer
        /// before it is denied (default 60). 0 waits however long the client
        /// takes - for a client that bounds the wait itself.
        #[arg(long, value_name = "SECS")]
        permission_timeout_secs: Option<u64>,
    },
}

/// Holds `config` to the budgets a parent handed down: each only ever
/// lowers what the configuration allows. The output cap is a bound on the
/// served model's own cap - configured, or its catalog entry's - never a
/// replacement for it.
fn apply_budgets(
    config: &mut sven_config::Config,
    max_tool_rounds: Option<u32>,
    max_output_tokens: Option<u32>,
    disable_tools: &[String],
) {
    if let Some(rounds) = max_tool_rounds {
        config.agent.max_tool_rounds = config.agent.max_tool_rounds.min(rounds);
    }
    if let Some(tokens) = max_output_tokens {
        let own = config.model.max_output_tokens.or_else(|| {
            sven_model::catalog::lookup(&config.model.provider, &config.model.name)
                .map(|entry| entry.max_output_tokens)
        });
        config.model.max_output_tokens = Some(own.map_or(tokens, |own| own.min(tokens)));
    }
    for name in disable_tools {
        if !config.tools.disabled.contains(name) {
            config.tools.disabled.push(name.clone());
        }
    }
}

/// Runs a `sven acp` subcommand.
pub async fn run_acp_command(cmd: &AcpCommands) -> anyhow::Result<()> {
    match cmd {
        AcpCommands::Serve {
            model,
            provider,
            max_tool_rounds,
            max_output_tokens,
            wall_clock_secs,
            disable_tools,
            permission_timeout_secs,
        } => {
            let mut config = sven_config::load(None)?;
            if let Some(ref name) = model {
                config.model = sven_model::resolve_model_from_config(&config, name);
            }
            if let Some(ref prov) = provider {
                config.model.provider = prov.clone();
            }
            apply_budgets(
                &mut config,
                *max_tool_rounds,
                *max_output_tokens,
                disable_tools,
            );
            let permission_timeout = match permission_timeout_secs {
                Some(0) => None,
                Some(secs) => Some(std::time::Duration::from_secs(*secs)),
                None => Some(crate::agent::DEFAULT_PERMISSION_TIMEOUT),
            };
            let serving = crate::serve_stdio_with(std::sync::Arc::new(config), permission_timeout);
            match wall_clock_secs {
                Some(secs) => tokio::time::timeout(std::time::Duration::from_secs(*secs), serving)
                    .await
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("wall-clock budget of {secs}s spent"))),
                None => serving.await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        acp: AcpCommands,
    }

    #[test]
    fn a_parent_budget_reaches_the_served_config_and_only_lowers_it() {
        let cli = Cli::try_parse_from([
            "sven",
            "serve",
            "--max-tool-rounds",
            "7",
            "--max-output-tokens",
            "2048",
            "--wall-clock-secs",
            "60",
            "--disable-tool",
            "shell",
        ])
        .expect("the flags parse");
        let AcpCommands::Serve {
            max_tool_rounds,
            max_output_tokens,
            wall_clock_secs,
            disable_tools,
            ..
        } = cli.acp;
        assert_eq!(wall_clock_secs, Some(60));

        let mut config = sven_config::Config::default();
        config.model.max_output_tokens = Some(8192);
        config.tools.disabled = vec!["web_fetch".into()];
        apply_budgets(
            &mut config,
            max_tool_rounds,
            max_output_tokens,
            &disable_tools,
        );
        assert_eq!(config.agent.max_tool_rounds, 7);
        assert_eq!(config.model.max_output_tokens, Some(2048));
        assert_eq!(
            config.tools.disabled,
            ["web_fetch", "shell"],
            "disabling only adds"
        );

        let mut config = sven_config::Config::default();
        config.agent.max_tool_rounds = 3;
        config.model.max_output_tokens = Some(1000);
        apply_budgets(&mut config, Some(50), Some(8000), &[]);
        assert_eq!(config.agent.max_tool_rounds, 3);
        assert_eq!(config.model.max_output_tokens, Some(1000));
    }

    #[test]
    fn a_parent_output_cap_never_raises_the_served_model_catalog_cap() {
        let entry = sven_model::catalog::static_catalog()
            .into_iter()
            .next()
            .expect("the catalog lists a model");
        let mut config = sven_config::Config::default();
        config.model.provider = entry.provider.clone();
        config.model.name = entry.id.clone();
        config.model.max_output_tokens = None;
        apply_budgets(&mut config, None, Some(u32::MAX), &[]);
        assert_eq!(
            config.model.max_output_tokens,
            Some(entry.max_output_tokens)
        );
    }
}
