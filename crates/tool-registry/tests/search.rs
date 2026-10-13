// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: searching a large set of overlapping tools finds the one that was
//! meant, and finds it the same way whatever order the tools come in.

use sven_hsm::ToolCapability;
use sven_tool_registry::{Bm25Ranker, ToolDescriptor, ToolRanker};

const VERBS: [(&str, &str); 10] = [
    ("read", "Get the details of one {} by id."),
    ("list", "Show every {} in the workspace, newest first."),
    ("search", "Find an {} by words in its text."),
    ("create", "Add a new {} to the workspace."),
    ("update", "Change the fields of an existing {}."),
    ("delete", "Remove an {} permanently."),
    ("export", "Save an {} to a file for sharing."),
    ("import", "Load an {} from a file."),
    ("archive", "Move an {} out of the active workspace."),
    ("restore", "Bring a deleted {} back."),
];
const OBJECTS: [&str; 10] = [
    "user", "order", "invoice", "ticket", "product", "report", "config", "session", "device", "log",
];

/// Ten verbs over ten objects: every tool shares most of its words with nine
/// others on each axis.
fn hundred_tools() -> Vec<ToolDescriptor> {
    VERBS
        .iter()
        .flat_map(|(verb, summary)| {
            OBJECTS.iter().map(move |object| ToolDescriptor {
                name: format!("{verb}_{object}"),
                summary: summary.replace("{}", object),
                capability: ToolCapability::NetworkAccess,
                mcp: true,
            })
        })
        .collect()
}

/// A fixed shuffle: the same permutation on every run.
fn permuted(tools: &[ToolDescriptor], seed: u64) -> Vec<ToolDescriptor> {
    let mut out = tools.to_vec();
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    for i in (1..out.len()).rev() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.swap(i, (state >> 33) as usize % (i + 1));
    }
    out
}

fn top(query: &str, tools: &[ToolDescriptor]) -> Vec<String> {
    Bm25Ranker
        .rank(query, tools)
        .into_iter()
        .map(|r| r.name)
        .collect()
}

#[test]
fn paraphrased_queries_find_the_tool_that_was_meant() {
    let tools = hundred_tools();
    for (query, wanted) in [
        ("remove a ticket for good", "delete_ticket"),
        ("get the details of an order by its id", "read_order"),
        ("find an invoice by words in it", "search_invoice"),
        ("bring a deleted device back", "restore_device"),
        ("save the report to a file", "export_report"),
        ("load a config from a file", "import_config"),
        ("add a new product", "create_product"),
        ("show every session", "list_session"),
        ("change fields of the user", "update_user"),
        ("move a log out of the active workspace", "archive_log"),
    ] {
        let ranked = top(query, &tools);
        assert_eq!(
            ranked.first().map(String::as_str),
            Some(wanted),
            "{query:?} ranked {:?}",
            &ranked[..ranked.len().min(4)]
        );
    }
}

#[test]
fn the_ranking_does_not_depend_on_the_order_the_tools_arrive_in() {
    let tools = hundred_tools();
    for query in [
        "delete a ticket",
        "export every report",
        "user",
        "find an order by text",
    ] {
        let reference = Bm25Ranker.rank(query, &tools);
        assert!(!reference.is_empty(), "{query:?} matched nothing");
        for seed in 1..=8 {
            assert_eq!(
                Bm25Ranker.rank(query, &permuted(&tools, seed)),
                reference,
                "{query:?} ranked differently for permutation {seed}"
            );
        }
    }
}

#[test]
fn equal_scores_are_ordered_by_name() {
    let tools = hundred_tools();
    // Every `read_*` summary has the same shape, so only the object word can
    // separate them: asking for the verb alone leaves them tied.
    let ranked = top("get one by id", &tools);
    let reads: Vec<&String> = ranked.iter().filter(|n| n.starts_with("read_")).collect();
    let mut sorted = reads.clone();
    sorted.sort();
    assert_eq!(reads, sorted);
}

#[test]
fn a_query_that_matches_nothing_returns_nothing() {
    assert!(top("zzz qqq", &hundred_tools()).is_empty());
    assert!(top("the of and", &hundred_tools()).is_empty());
}
