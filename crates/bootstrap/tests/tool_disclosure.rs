// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in deferred tool
// disclosure for agents then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: over the tools a coding session really has, deferred disclosure
//! describes every tool in one line, finds the tool a paraphrased request is
//! about, loads what is asked for and nothing else, and leaves the legacy
//! full listing exactly as it was.

use std::sync::Arc;

use serde_json::json;

use sven_bootstrap::tool_discovery::index_section;
use sven_bootstrap::{BuiltinTools, Config, Disclosure, RuntimeBuilder};
use sven_tool_registry::{ToolCall, ToolRegistry, DISCOVERY_TOOL};
use sven_vocab::AgentMode;

/// Points the process at one private home directory, once. Skills, personas
/// and memory come from the home directory, and tests run in parallel: a home
/// switched per test would let one test's registry lose a tool (the semantic
/// memory store opens under it) while another's keeps it.
fn isolate_home() {
    static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("a home directory").keep();
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        home
    });
}

/// A coding session's registry under `disclosure`.
async fn registry(disclosure: Disclosure) -> Arc<ToolRegistry> {
    isolate_home();
    let mut config = Config::default();
    config.model.provider = "mock".into();
    config.model.name = "mock-model".into();
    config.tools.disclosure.mode = disclosure;
    let (runtime, handle, ..) = RuntimeBuilder::new(Arc::new(config), "agent")
        .with_builtin_tools(BuiltinTools::Coding)
        .build()
        .await
        .expect("a runtime builds");
    runtime.abort();
    handle.tool_registry()
}

async fn run(registry: &ToolRegistry, args: serde_json::Value) -> sven_tool_registry::ToolOutput {
    let discovery = registry.get(DISCOVERY_TOOL).expect("the discovery tool");
    discovery
        .execute(&ToolCall {
            id: "t".into(),
            name: DISCOVERY_TOOL.into(),
            args,
        })
        .await
}

fn tokens(schemas: &[sven_tool_registry::ToolSchema]) -> usize {
    schemas
        .iter()
        .map(|s| (s.name.len() + s.description.len() + s.parameters.to_string().len()).div_ceil(4))
        .sum()
}

#[tokio::test]
async fn the_legacy_full_listing_is_untouched_and_has_no_discovery_tool() {
    let registry = registry(Disclosure::Full).await;
    assert!(registry.get(DISCOVERY_TOOL).is_none());
    let legacy = registry.schemas_for_mode(AgentMode::Agent);
    assert_eq!(
        registry.schemas_offered(&[], "agent", &["task".into()]),
        legacy
    );
}

#[tokio::test]
async fn the_deferred_core_is_a_fraction_of_the_full_listing() {
    let full = registry(Disclosure::Full).await;
    let deferred = registry(Disclosure::Deferred).await;
    // Compare over the tools both registries hold: an optional tool (the
    // semantic memory store) that registers in one session and not the other
    // would otherwise move the ratio. The discovery tool exists only under
    // deferred disclosure and is part of its cost.
    let shared = |name: &str| {
        name == DISCOVERY_TOOL || (full.get(name).is_some() && deferred.get(name).is_some())
    };
    let offered = |registry: &ToolRegistry| {
        let mut schemas = registry.schemas_offered(&[], "agent", &[]);
        schemas.retain(|s| shared(&s.name));
        tokens(&schemas)
    };
    let full_tokens = offered(&full);
    let core_tokens = offered(&deferred);
    assert!(
        core_tokens * 2 < full_tokens,
        "core {core_tokens} vs full {full_tokens} estimated tokens"
    );
}

#[tokio::test]
async fn the_system_prompt_index_names_every_deferred_tool_once() {
    let registry = registry(Disclosure::Deferred).await;
    let index = index_section(&registry, AgentMode::Agent).expect("an index");
    for name in [
        "task",
        "web_fetch",
        "web_search",
        "skill",
        "system",
        "memory",
    ] {
        assert_eq!(
            index.matches(&format!("- {name}: ")).count(),
            1,
            "{name} in {index}"
        );
    }
    for core in ["shell", "read_file", "grep"] {
        assert!(!index.contains(&format!("- {core}: ")), "{core} is core");
    }
    assert!(index.contains("action=load"));
}

#[tokio::test]
async fn full_disclosure_has_no_index() {
    let registry = registry(Disclosure::Full).await;
    assert!(index_section(&registry, AgentMode::Agent).is_none());
}

#[tokio::test]
async fn paraphrased_requests_find_the_tool_they_are_about() {
    let registry = registry(Disclosure::Deferred).await;
    for (query, wanted) in [
        ("fetch the text of a web page from a url", "web_fetch"),
        ("search the web for recent news", "web_search"),
        ("delegate work to a sub-agent", "task"),
        ("switch to a different model", "system"),
        ("save a note by key", "memory"),
        ("ask the user a multiple choice question", "ask_question"),
        ("attach an image to the conversation", "attach_file"),
        ("load a skill", "skill"),
    ] {
        let out = run(&registry, json!({"action": "search", "query": query})).await;
        assert!(!out.is_error, "{query}: {}", out.content);
        let first = out
            .content
            .lines()
            .find(|l| l.starts_with("- "))
            .unwrap_or_default();
        assert!(
            first.starts_with(&format!("- {wanted}:")),
            "{query:?} ranked {first:?} first:\n{}",
            out.content
        );
    }
}

#[tokio::test]
async fn load_names_only_what_can_be_activated() {
    let registry = registry(Disclosure::Deferred).await;
    let call = ToolCall {
        id: "t".into(),
        name: DISCOVERY_TOOL.into(),
        args: json!({"action": "load", "names": ["web_fetch", "shell", "web_fetchh", "web_fetch"]}),
    };
    assert_eq!(registry.activations(&call), ["web_fetch"]);

    let out = run(&registry, call.args.clone()).await;
    assert!(!out.is_error);
    assert!(out.content.contains("Loaded: web_fetch"), "{}", out.content);
    assert!(
        out.content.contains("did you mean web_fetch"),
        "{}",
        out.content
    );
    assert!(
        out.content.contains("Already in your tool list: shell"),
        "{}",
        out.content
    );
}

#[tokio::test]
async fn loading_nothing_valid_is_an_error_and_activates_nothing() {
    let registry = registry(Disclosure::Deferred).await;
    let args = json!({"action": "load", "names": ["no_such_tool"]});
    let out = run(&registry, args.clone()).await;
    assert!(out.is_error, "{}", out.content);
    let call = ToolCall {
        id: "t".into(),
        name: DISCOVERY_TOOL.into(),
        args,
    };
    assert!(registry.activations(&call).is_empty());
}

#[tokio::test]
async fn a_missed_search_is_never_a_dead_end() {
    let registry = registry(Disclosure::Deferred).await;
    let miss = run(&registry, json!({"action": "search", "query": "zzzqqq"})).await;
    assert!(miss.content.contains("action=list"), "{}", miss.content);

    let page = run(&registry, json!({"action": "list"})).await;
    assert!(page.content.contains("web_fetch"), "{}", page.content);
    assert!(
        page.content.contains("That is all of them"),
        "{}",
        page.content
    );

    let past = run(&registry, json!({"action": "list", "offset": 500})).await;
    assert!(past.is_error);
}

#[tokio::test]
async fn help_shows_a_definition_without_activating_it() {
    let registry = registry(Disclosure::Deferred).await;
    let args = json!({"action": "help", "names": ["web_fetch"]});
    let out = run(&registry, args.clone()).await;
    assert!(out.content.contains("parameters:"), "{}", out.content);
    let call = ToolCall {
        id: "t".into(),
        name: DISCOVERY_TOOL.into(),
        args,
    };
    assert!(registry.activations(&call).is_empty());
}
