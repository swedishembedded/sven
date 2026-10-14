// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The stub MCP server of the tool-selection suite.

use serde_json::json;
use sven_ablation::mcp_stub::{handle, serve};
use sven_ablation::task::CatalogTool;

fn catalog() -> Vec<CatalogTool> {
    vec![
        CatalogTool {
            name: "billing_invoice_create".into(),
            description: "Creates a new invoice.".into(),
        },
        CatalogTool {
            name: "billing_invoice_list".into(),
            description: "Lists the invoice.".into(),
        },
    ]
}

#[test]
fn it_initializes_lists_and_calls() {
    let c = catalog();
    let init = handle(
        &c,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    )
    .unwrap();
    assert_eq!(init["id"], 1);
    assert!(init["result"]["capabilities"]["tools"].is_object());

    let list = handle(&c, &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "billing_invoice_create");
    assert_eq!(tools[0]["inputSchema"]["type"], "object");

    let call = handle(
        &c,
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"billing_invoice_list","arguments":{}}}),
    )
    .unwrap();
    assert_eq!(
        call["result"]["content"][0]["text"],
        "ok: billing_invoice_list"
    );
}

#[test]
fn it_refuses_an_unknown_tool_and_an_unknown_method() {
    let c = catalog();
    let call = handle(
        &c,
        &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope"}}),
    )
    .unwrap();
    assert!(call["error"]["message"].as_str().unwrap().contains("nope"));
    let other = handle(
        &c,
        &json!({"jsonrpc":"2.0","id":5,"method":"resources/list"}),
    )
    .unwrap();
    assert_eq!(other["error"]["code"], -32601);
}

#[test]
fn a_notification_gets_no_answer_and_the_loop_ends_with_its_input() {
    let c = catalog();
    assert!(handle(
        &c,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .is_none());
    let input = b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n";
    let mut out = Vec::new();
    serve(&c, &input[..], &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains("\"id\":9"));
}
