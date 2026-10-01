// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use sven_model::catalog::ModelCatalogEntry;

/// List available models, optionally querying the provider API for live data.
pub(crate) async fn list_models_cmd(
    config: &sven_bootstrap::Config,
    provider_filter: Option<&str>,
    refresh: bool,
    as_json: bool,
) -> anyhow::Result<()> {
    // Validate provider filter against the registry.
    if let Some(prov) = provider_filter {
        if sven_model::get_driver(prov).is_none() {
            eprintln!("Unknown provider: {prov:?}");
            eprintln!("\nAvailable providers (run `sven list-providers` for details):");
            for d in sven_model::list_drivers() {
                eprintln!("  {:20} {}", d.id, d.name);
            }
            anyhow::bail!("Invalid provider: {prov}");
        }
    }

    let entries: Vec<ModelCatalogEntry> = if refresh {
        // Query the configured (or filtered) provider's live API.
        let model_cfg = if let Some(prov) = provider_filter {
            let mut c = config.model.clone();
            c.provider = prov.to_string();
            c
        } else {
            config.model.clone()
        };
        let model = sven_model_drivers::from_config(&model_cfg)?;
        let mut live = model.list_models().await?;
        if let Some(prov) = provider_filter {
            live.retain(|e| e.provider == prov);
        }
        live
    } else {
        // Use static catalog only.
        let mut all = sven_model::catalog::static_catalog();
        if let Some(prov) = provider_filter {
            all.retain(|e| e.provider == prov);
        }
        all.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));
        all
    };

    if as_json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("No models found.");
        return Ok(());
    }

    // Determine column widths.
    let id_w = entries
        .iter()
        .map(|e| e.id.len())
        .max()
        .unwrap_or(10)
        .max(10);
    let prov_w = entries
        .iter()
        .map(|e| e.provider.len())
        .max()
        .unwrap_or(8)
        .max(8);

    println!(
        "{:<id_w$}  {:<prov_w$}  {:>12}  {:>16}  DESCRIPTION",
        "ID",
        "PROVIDER",
        "CTX WINDOW",
        "MAX OUT TOKENS",
        id_w = id_w,
        prov_w = prov_w,
    );
    println!("{}", "-".repeat(id_w + prov_w + 50));

    for e in &entries {
        let ctx = if e.context_window == 0 {
            "  -".to_string()
        } else {
            format!("{:>12}", e.context_window)
        };
        let max_out = if e.max_output_tokens == 0 {
            "  -".to_string()
        } else {
            format!("{:>16}", e.max_output_tokens)
        };
        println!(
            "{:<id_w$}  {:<prov_w$}  {}  {}  {}",
            e.id,
            e.provider,
            ctx,
            max_out,
            e.description,
            id_w = id_w,
            prov_w = prov_w,
        );
    }
    println!("\nTotal: {} model(s)", entries.len());
    Ok(())
}

/// List all registered model providers.
pub(crate) fn list_providers_cmd(verbose: bool, as_json: bool) -> anyhow::Result<()> {
    let drivers = sven_model::list_drivers();

    if as_json {
        #[derive(serde::Serialize)]
        struct ProviderJson {
            id: &'static str,
            name: &'static str,
            description: &'static str,
            default_api_key_env: Option<&'static str>,
            default_base_url: Option<&'static str>,
            requires_api_key: bool,
        }
        let rows: Vec<ProviderJson> = drivers
            .iter()
            .map(|d| ProviderJson {
                id: d.id,
                name: d.name,
                description: d.description,
                default_api_key_env: d.default_api_key_env,
                default_base_url: d.default_base_url,
                requires_api_key: d.requires_api_key,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!("Supported Model Providers ({} total)\n", drivers.len());

    if verbose {
        for d in drivers {
            println!("  {} - {}", d.id, d.name);
            println!("    {}", d.description);
            if let Some(env) = d.default_api_key_env {
                println!("    API key env : {env}");
            }
            if let Some(url) = d.default_base_url {
                println!("    Default URL : {url}");
            }
            if !d.requires_api_key {
                println!("    Auth        : none required");
            }
            println!();
        }
    } else {
        let id_w = drivers
            .iter()
            .map(|d| d.id.len())
            .max()
            .unwrap_or(10)
            .max(10);
        let name_w = drivers
            .iter()
            .map(|d| d.name.len())
            .max()
            .unwrap_or(8)
            .max(8);
        println!("{:<id_w$}  {:<name_w$}  DESCRIPTION", "ID", "NAME");
        println!("{}", "-".repeat(id_w + name_w + 40));
        for d in drivers {
            println!("{:<id_w$}  {:<name_w$}  {}", d.id, d.name, d.description);
        }
        println!("\nUse `sven list-providers --verbose` for API key and URL details.");
        println!("Use `sven list-models --provider <ID>` to see models for a specific provider.");
    }
    Ok(())
}
