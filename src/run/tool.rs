// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;

use crate::cli::ToolCommands;
use sven_bootstrap::build_cli_tool_registry;

// ── Tool command handler ──────────────────────────────────────────────────────

pub(crate) async fn run_tool_command(
    cmd: &ToolCommands,
    cfg: &sven_bootstrap::Config,
) -> anyhow::Result<()> {
    use sven_tool_api::tool::ToolCall;

    match cmd {
        // ── sven tool list ────────────────────────────────────────────────────
        ToolCommands::List => {
            let reg = build_cli_tool_registry(cfg);
            let mut schemas = reg.schemas();
            schemas.sort_by(|a, b| a.name.cmp(&b.name));

            let name_width = schemas.iter().map(|s| s.name.len()).max().unwrap_or(0) + 2;
            println!("Built-in tools ({} total):\n", schemas.len());
            for s in &schemas {
                // Grab the first sentence of the description for the summary line.
                let summary = s
                    .description
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                println!("  {:<width$}  {}", s.name, summary, width = name_width);
            }
            println!("\nRun 'sven tool call <TOOL> --help' to see a tool's parameters.");
            Ok(())
        }

        // ── sven tool call [<TOOL> [key=value ...]] ───────────────────────────
        ToolCommands::Call { args } => {
            // Parse the flat args vector:
            //   []                  → show all tools + schemas
            //   ["--help"|"-h"]     → show all tools + schemas
            //   ["<TOOL>"]          → show that tool's schema
            //   ["<TOOL>","--help"] → show that tool's schema
            //   ["<TOOL>", ...]     → execute

            let reg = build_cli_tool_registry(cfg);

            // Strip leading --help / -h flags to determine whether a tool name
            // was provided.
            let stripped: Vec<&String> = args
                .iter()
                .filter(|a| *a != "--help" && *a != "-h")
                .collect();

            if stripped.is_empty() {
                // No tool name → print all tools with their full schemas.
                print_all_tools_help(&reg);
                return Ok(());
            }

            let tool_name = stripped[0];
            let tool_schema = match reg.schemas().into_iter().find(|s| &s.name == tool_name) {
                Some(s) => s,
                None => {
                    let mut available = reg.names();
                    available.sort();
                    eprintln!("error: unknown tool '{tool_name}'");
                    eprintln!("\nAvailable tools (use 'sven tool list' for details):");
                    for name in &available {
                        eprintln!("  {name}");
                    }
                    std::process::exit(2);
                }
            };

            // Remaining args after the tool name (still including any --help).
            let rest: Vec<&String> = args[1..].iter().collect();

            // No params, or only --help / -h → show tool schema.
            let only_help = rest.iter().all(|a| *a == "--help" || *a == "-h");
            if rest.is_empty() || only_help {
                print_tool_help(
                    &tool_schema.name,
                    &tool_schema.description,
                    &tool_schema.parameters,
                );
                return Ok(());
            }

            // ── Build args JSON ───────────────────────────────────────────────
            let json_flag_pos = rest.iter().position(|a| *a == "--json");
            let args_value: serde_json::Value = if let Some(pos) = json_flag_pos {
                let raw = rest
                    .get(pos + 1)
                    .with_context(|| "--json requires a JSON string argument")?;
                serde_json::from_str(raw)
                    .with_context(|| format!("--json value is not valid JSON: {raw}"))?
            } else {
                let kv: Vec<String> = rest
                    .iter()
                    .filter(|a| a.as_str() != "--help" && a.as_str() != "-h")
                    .map(|s| (*s).clone())
                    .collect();
                parse_kv_args(&kv)?
            };

            // ── Execute ───────────────────────────────────────────────────────
            let call = ToolCall {
                id: "cli".to_string(),
                name: tool_name.clone(),
                args: args_value,
            };

            let output = reg.execute(&call).await;

            if output.is_error {
                eprintln!("error: {}", output.content);
                std::process::exit(1);
            } else {
                println!("{}", output.content);
            }
            Ok(())
        }
    }
}

/// Parse `key=value` strings into a `serde_json::Value::Object`.
///
/// Value type inference:
/// - `"true"` / `"false"` → `bool`
/// - All-digit strings → `i64`
/// - Everything else → `String`
pub(crate) fn parse_kv_args(params: &[String]) -> anyhow::Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for param in params {
        let (key, val_str) = param
            .split_once('=')
            .with_context(|| format!("argument '{param}' is not in key=value form"))?;
        let val = match val_str {
            "true" => serde_json::Value::Bool(true),
            "false" => serde_json::Value::Bool(false),
            s if s.parse::<i64>().is_ok() => {
                serde_json::Value::Number(s.parse::<i64>().unwrap().into())
            }
            s => serde_json::Value::String(s.to_string()),
        };
        map.insert(key.to_string(), val);
    }
    Ok(serde_json::Value::Object(map))
}

/// Print every tool's name + full parameter schema.
///
/// This is what `sven tool call --help` (and `sven tool call` with no args)
/// displays: a complete reference for all available tools.
pub(crate) fn print_all_tools_help(reg: &sven_tool_registry::ToolRegistry) {
    let mut schemas = reg.schemas();
    schemas.sort_by(|a, b| a.name.cmp(&b.name));
    println!("sven built-in tools ({} total)\n", schemas.len());
    println!(
        "Usage:\n  sven tool call <TOOL> [key=value ...]   - execute a tool\n  \
         sven tool call <TOOL>                    - show that tool's schema\n  \
         sven tool call <TOOL> --json '{{...}}'   - pass raw JSON args\n  \
         sven tool list                           - compact name+description list\n"
    );
    println!("{}\n", "─".repeat(72));
    for schema in &schemas {
        print_tool_help(&schema.name, &schema.description, &schema.parameters);
        println!("{}\n", "─".repeat(72));
    }
}

/// Print a human-readable parameter schema for one tool.
pub(crate) fn print_tool_help(name: &str, description: &str, schema: &serde_json::Value) {
    println!("Tool: {name}\n");
    println!("{description}\n");

    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        println!("(no parameters)");
        return;
    };

    // Collect required fields.
    let required: std::collections::HashSet<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let name_width = props.keys().map(|k| k.len()).max().unwrap_or(0) + 2;
    println!("Parameters:");
    for (key, prop) in props {
        let typ = prop
            .get("type")
            .and_then(|t| t.as_str())
            .or_else(|| prop.get("enum").map(|_| "enum"))
            .unwrap_or("any");
        let desc = prop
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        let req_marker = if required.contains(key.as_str()) {
            "*"
        } else {
            " "
        };

        // Show enum values if present.
        let enum_hint = if let Some(vals) = prop.get("enum").and_then(|v| v.as_array()) {
            let options: Vec<&str> = vals.iter().filter_map(|v| v.as_str()).collect();
            format!(" [{}]", options.join(" | "))
        } else {
            String::new()
        };

        println!(
            "  {req_marker} {:<width$}  ({typ}{enum_hint})  {desc}",
            key,
            width = name_width
        );
    }
    println!("\n  * = required");
    println!("\nUsage:");
    println!("  sven tool call {name} key=value ...");
    println!("  sven tool call {name} --json '{{\"key\": \"value\"}}'");
}
