// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use crate::cli::{NodeCommands, WebDevicesCommands};

// ── Node command handler ──────────────────────────────────────────────────────

pub(crate) async fn run_node_command(cmd: &NodeCommands) -> anyhow::Result<()> {
    match cmd {
        NodeCommands::Start {
            config: config_path,
            model: model_override,
            provider: provider_override,
            pty_insecure,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            let mut sven_config = sven_config::load(None)?;

            // Apply CLI model/provider overrides on top of whatever was loaded
            // from the config file.  Precedence: CLI flag > env var (SVEN_MODEL
            // is already wired through clap) > config file > auto-detected.
            if let Some(ref name) = model_override {
                sven_config.model = sven_model::resolve_model_from_config(&sven_config, name);
            }
            if let Some(ref prov) = provider_override {
                sven_config.model.provider = prov.clone();
            }

            let sven_config = Arc::new(sven_config);
            if let Err(e) = sven_node::node::run(node_config, sven_config, *pty_insecure).await {
                tracing::error!("{e:#}");
                std::process::exit(1);
            }
            Ok(())
        }

        NodeCommands::Authorize {
            uri,
            label,
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::node::pair_peer(&node_config, uri, label.clone()).await
        }

        NodeCommands::Revoke {
            peer_id,
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::node::revoke_peer(&node_config, peer_id).await
        }

        NodeCommands::RegenerateToken {
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::node::regenerate_token(&node_config)
        }

        NodeCommands::ShowConfig {
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            println!(
                "{}",
                serde_yaml::to_string(&node_config).unwrap_or_default()
            );
            Ok(())
        }

        NodeCommands::ListOperators {
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::list_peers(&node_config)
        }

        NodeCommands::Exec {
            task,
            token,
            url,
            config: config_path,
            insecure,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::exec_task(&node_config, url, token, task, *insecure).await
        }

        NodeCommands::WebDevices { command } => run_web_devices_command(command).await,

        NodeCommands::InstallCa {
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            let cert_dir = node_config
                .http
                .tls_cert_dir
                .clone()
                .unwrap_or_else(sven_node::tls_default_cert_dir);
            match sven_node::export_ca_cert(&cert_dir)? {
                None => {
                    eprintln!(
                        "No local CA found in {}.\n\
                         Start the node once with tls_mode: local-ca (or auto) to generate it.",
                        cert_dir.display()
                    );
                    std::process::exit(1);
                }
                Some(_) => {
                    sven_node::print_install_instructions(&cert_dir.join("ca-cert.pem"));
                    Ok(())
                }
            }
        }

        NodeCommands::ExportCa {
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            let cert_dir = node_config
                .http
                .tls_cert_dir
                .clone()
                .unwrap_or_else(sven_node::tls_default_cert_dir);
            match sven_node::export_ca_cert(&cert_dir)? {
                None => {
                    eprintln!(
                        "No local CA found in {}.\n\
                         Start the node once with tls_mode: local-ca (or auto) to generate it.",
                        cert_dir.display()
                    );
                    std::process::exit(1);
                }
                Some(pem) => {
                    print!("{pem}");
                    Ok(())
                }
            }
        }
    }
}

pub(crate) async fn run_web_devices_command(cmd: &WebDevicesCommands) -> anyhow::Result<()> {
    match cmd {
        WebDevicesCommands::List {
            filter,
            token,
            url,
            config: config_path,
            insecure,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::web_devices_list(&node_config, url, token, filter, *insecure).await
        }
        WebDevicesCommands::Approve {
            device_id,
            token,
            url,
            config: config_path,
            insecure,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::web_devices_approve(&node_config, url, token, device_id, *insecure).await
        }
        WebDevicesCommands::Revoke {
            device_id,
            token,
            url,
            config: config_path,
            insecure,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::web_devices_revoke(&node_config, url, token, device_id, *insecure).await
        }
    }
}
