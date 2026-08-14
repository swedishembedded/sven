// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;

use sven_session_store::{parse_frontmatter, parse_workflow};

/// Validate a workflow file: parse frontmatter, count steps, report to stdout.
pub(crate) fn validate_workflow(file: &std::path::Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("reading workflow file {}", file.display()))?;

    let (frontmatter, markdown_body) = parse_frontmatter(&content);

    let workflow = parse_workflow(markdown_body);

    // Title: frontmatter overrides H1
    let title = frontmatter
        .as_ref()
        .and_then(|fm| fm.title.as_deref())
        .or(workflow.title.as_deref());
    if let Some(t) = title {
        println!("Title: {t}");
    }

    if let Some(fm) = &frontmatter {
        println!("Frontmatter: OK");
        if let Some(models) = &fm.models {
            println!("  models ({}):", models.len());
            let mut pairs: Vec<_> = models.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            for (mode, model) in pairs {
                println!("    {mode}: {model}");
            }
        }
        if let Some(vars) = &fm.vars {
            println!("  vars ({}):", vars.len());
            let mut pairs: Vec<_> = vars.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            for (k, v) in pairs {
                println!("    {k} = {v}");
            }
        }
    } else {
        println!("Frontmatter: (none)");
    }

    if let Some(preamble) = &workflow.system_prompt_append {
        println!(
            "Preamble: {} chars (appended to system prompt)",
            preamble.chars().count()
        );
    }

    let mut queue = workflow.steps;
    let total = queue.len();
    println!("Steps: {total}");

    let mut i = 0;
    while let Some(step) = queue.pop() {
        i += 1;
        let label = step.label.as_deref().unwrap_or("(unlabelled)");
        let mode = step.options.mode.as_deref().unwrap_or("(inherit)");
        let provider = step.options.provider.as_deref().unwrap_or("(inherit)");
        let model = step.options.model.as_deref().unwrap_or("(inherit)");
        let timeout = step
            .options
            .timeout_secs
            .map(|t| format!("{t}s"))
            .unwrap_or_else(|| "(inherit)".to_string());
        println!("  Step {i}/{total}: {label:?}  mode={mode}  provider={provider}  model={model}  timeout={timeout}");
        if !step.content.is_empty() {
            let preview = step.content.chars().take(80).collect::<String>();
            let ellipsis = if step.content.chars().count() > 80 {
                "..."
            } else {
                ""
            };
            println!("    {preview}{ellipsis}");
        }
    }

    println!("\nWorkflow is valid.");
    Ok(())
}
