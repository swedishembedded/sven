// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use sven_model::catalog::ModelCatalogEntry;

/// `sven list-models`: the models that can be used now, asked live; or, with
/// `catalog`, the bundled catalog.
pub(crate) async fn list_models_cmd(
    config: &sven_bootstrap::Config,
    provider_filter: Option<&str>,
    catalog: bool,
    as_json: bool,
) -> anyhow::Result<()> {
    if let Some(prov) = provider_filter {
        if sven_model::get_driver(prov).is_none() && !config.providers.contains_key(prov) {
            eprintln!("Unknown provider: {prov:?}");
            eprintln!("\nA driver id (`sven list-providers`) or a `providers:` entry of the configuration:");
            for d in sven_model::list_drivers() {
                eprintln!("  {:20} {}", d.id, d.name);
            }
            let mut aliases: Vec<&String> = config.providers.keys().collect();
            aliases.sort();
            for alias in aliases {
                eprintln!("  {alias:20} (providers: entry)");
            }
            anyhow::bail!("Invalid provider: {prov}");
        }
    }
    if catalog {
        return print_catalog(provider_filter, as_json);
    }

    let mut found = sven_model_drivers::reachable::reachable_models(&config.providers).await;
    if let Some(prov) = provider_filter {
        found.models.retain(|m| m.source == prov);
        found.problems.retain(|p| p.source == prov);
    }
    if as_json {
        println!("{}", serde_json::to_string_pretty(&found)?);
        return Ok(());
    }
    for problem in &found.problems {
        eprintln!("{}: {}", problem.source, problem.problem);
    }
    if found.models.is_empty() {
        println!("No model can be used now. Start a local server (`brain serve`), or store a hosted provider's key (`sven auth set openrouter`).");
        println!("`sven list-models --catalog` shows the bundled catalog.");
        return Ok(());
    }
    let ref_w = found
        .models
        .iter()
        .map(|m| m.reference.len())
        .max()
        .unwrap_or(10)
        .max(10);
    println!(
        "{:<ref_w$}  {:>10}  {:>10}  TOOLS",
        "MODEL (--model)", "CONTEXT", "MAX OUT"
    );
    println!("{}", "-".repeat(ref_w + 32));
    let count = |n: Option<u32>| n.map_or_else(|| "-".to_string(), |n| n.to_string());
    for m in &found.models {
        let tools = match m.tools {
            Some(true) => "yes",
            Some(false) => "no",
            None => "?",
        };
        println!(
            "{:<ref_w$}  {:>10}  {:>10}  {tools}",
            m.reference,
            count(m.context_length),
            count(m.max_output_tokens)
        );
    }
    println!("\nTotal: {} model(s)", found.models.len());
    Ok(())
}

/// The bundled catalog, optionally of one driver.
fn print_catalog(provider_filter: Option<&str>, as_json: bool) -> anyhow::Result<()> {
    let mut entries: Vec<ModelCatalogEntry> = sven_model::catalog::static_catalog();
    if let Some(prov) = provider_filter {
        entries.retain(|e| e.provider == prov);
    }
    entries.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));

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
    let stored = sven_model_drivers::credentials::providers();

    if as_json {
        #[derive(serde::Serialize)]
        struct ProviderJson {
            id: &'static str,
            name: &'static str,
            description: &'static str,
            key_stored: bool,
            default_base_url: Option<&'static str>,
            requires_api_key: bool,
        }
        let rows: Vec<ProviderJson> = drivers
            .iter()
            .map(|d| ProviderJson {
                id: d.id,
                name: d.name,
                description: d.description,
                key_stored: stored.iter().any(|p| p == d.id),
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
            if d.requires_api_key {
                let key = if stored.iter().any(|p| p == d.id) {
                    "stored"
                } else {
                    "none (`sven auth set` stores one)"
                };
                println!("    Key         : {key}");
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
        println!("\nUse `sven list-providers --verbose` for key and URL details.");
        println!("Use `sven list-models --provider <ID>` to see the models a provider serves now.");
    }
    Ok(())
}
