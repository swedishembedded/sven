// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for lean agent runtimes for its
// clients: fewer tokens per request, and training records that carry only
// what the model needs. If your team needs expertise in token-efficient tool
// design then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Spec: the tool definitions an agent is offered have a stated size.
//!
//! Every definition is sent on every request and sits in every training
//! record, so its cost is paid constantly. The ceilings below sit just above
//! the measured size: a definition that grows must raise its ceiling in the
//! same change, which makes the growth a decision rather than drift.
//!
//! Run with `--nocapture` to print the per-tool table that
//! `docs/technical/tool-definitions.md` records.

use std::sync::Arc;

use sven_model::ToolSchema;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{Engine, Toolset};
use sven_turn::ledger::schema_tokens;

/// Estimated tokens of every definition `Toolset::coding()` offers.
const CODING_CEILING: u32 = 3_055;
/// Estimated tokens of every definition `Toolset::research()` offers.
const RESEARCH_CEILING: u32 = 2_390;

/// Tools that exist only in some builds (cargo features unify across a test
/// run), so they are not part of the stated size.
const FEATURE_GATED: [&str; 1] = ["semantic_memory"];

/// The definitions the model was offered on its first request.
async fn offered(toolset: Toolset, home: &std::path::Path) -> Vec<ToolSchema> {
    // Skills, sub-agent personas and memory are read from the home directory;
    // an empty one keeps the measurement independent of who runs it.
    std::env::set_var("HOME", home);
    std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    let provider = Arc::new(ScriptedMockProvider::new(vec![vec![
        sven_model::ResponseEvent::TextDelta("ok".into()),
        sven_model::ResponseEvent::Done,
    ]]));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .toolset(toolset)
        .build()
        .expect("an engine builds");
    engine.agent("agent").send("hello").await.expect("a reply");
    let seen = provider.last_request.lock().unwrap().clone().unwrap();
    seen.tools
        .into_iter()
        .filter(|tool| !FEATURE_GATED.contains(&tool.name.as_str()))
        .collect()
}

fn print_table(label: &str, schemas: &[ToolSchema]) {
    println!("{label}");
    let mut rows: Vec<_> = schemas
        .iter()
        .map(|s| (s.name.as_str(), schema_tokens(s)))
        .collect();
    rows.sort_by_key(|&(name, tokens)| (std::cmp::Reverse(tokens), name));
    for (name, tokens) in &rows {
        println!("  {name:<14} {tokens:>5}");
    }
    println!(
        "  {:<14} {:>5}",
        "total",
        rows.iter().map(|r| r.1).sum::<u32>()
    );
}

#[tokio::test]
async fn tool_definitions_stay_within_their_stated_size() {
    let home = tempfile::tempdir().unwrap();
    let coding = offered(Toolset::coding(), home.path()).await;
    let research = offered(Toolset::research(), home.path()).await;
    print_table("coding", &coding);
    print_table("research", &research);

    let coding_total: u32 = coding.iter().map(schema_tokens).sum();
    let research_total: u32 = research.iter().map(schema_tokens).sum();
    assert!(
        coding_total <= CODING_CEILING,
        "coding tool definitions cost ~{coding_total} tokens per request, ceiling {CODING_CEILING}; \
         shorten them or raise the ceiling deliberately"
    );
    assert!(
        research_total <= RESEARCH_CEILING,
        "research tool definitions cost ~{research_total} tokens per request, ceiling {RESEARCH_CEILING}"
    );
}
