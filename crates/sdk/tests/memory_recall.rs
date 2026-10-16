// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a real `semantic_memory` recall, not a stand-in tool, is what the
//! exported trajectory records as memory use: the step that made the call
//! carries `extra.sven.memory` with the query, the space, the index
//! generation and every unit the recall returned.
//!
//! Swedish Embedded AB implements auditable agent runtimes for its clients. If
//! your team needs expertise in tracing which memory an agent's answer came
//! from then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use sven_memory::{SemanticMemoryTool, PERSONAL_SPACE};
use sven_memory_sqlite::SqliteMemory;
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::atif::{validate_trajectory, MemoryRecord};
use sven_sdk::Engine;

fn call(id: &str, arguments: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: id.into(),
            name: "semantic_memory".into(),
            arguments: arguments.into(),
        },
        ResponseEvent::Done,
    ]
}

#[tokio::test]
async fn a_recall_through_the_real_memory_tool_is_in_the_exported_trajectory() {
    let scripts = vec![
        call(
            "w1",
            r#"{"action":"remember","content":"the launch code is bats-9182"}"#,
        ),
        call("r1", r#"{"action":"recall","query":"launch code"}"#),
        vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done],
    ];
    let store = Arc::new(SqliteMemory::open_in_memory().expect("an in-memory store"));
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .tool(Arc::new(SemanticMemoryTool::personal(store)))
        .build()
        .expect("an engine builds");

    let mut agent = engine.agent("agent");
    tokio::time::timeout(std::time::Duration::from_secs(20), agent.send("go"))
        .await
        .expect("the run ends")
        .expect("an outcome");

    let trajectory = agent.trajectory();
    assert_eq!(validate_trajectory(&trajectory), Ok(()));
    let retrievals: Vec<_> = trajectory
        .steps
        .iter()
        .filter_map(|s| MemoryRecord::from_extra(s.extra.as_ref()?))
        .flat_map(|record| record.retrievals)
        .collect();
    assert_eq!(retrievals.len(), 1, "{retrievals:?}");
    let retrieval = &retrievals[0];
    assert_eq!(retrieval.call_id, "r1", "the executor names the recall");
    assert_eq!(retrieval.query, "launch code");
    assert_eq!(retrieval.space_ids, [PERSONAL_SPACE]);
    assert_eq!(retrieval.generation_id, "lexical");
    assert_eq!(retrieval.results.len(), 1);
    assert_eq!(retrieval.results[0].rank, 1);
    assert!(retrieval.results[0].inserted);
}
