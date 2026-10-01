// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Giving an agent a capability the kernel has never heard of.
//!
//! The tool below lives in this file, not in sven. It is registered on the
//! engine and from then on is permission-gated and audited exactly like a
//! built-in one: `kernel_capability` decides which bucket it falls under, and
//! `default_policy` whether it needs approval before running.
//!
//! This is what makes sven a framework rather than a program: an application
//! extends it without editing it.
//!
//! Run with: `cargo run -p sven-sdk --example custom_tool`

use std::sync::Arc;

use sven_sdk::{
    tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    Engine,
};

/// Looks up a ticker in this application's own price service.
struct StockPrice;

#[async_trait::async_trait]
impl Tool for StockPrice {
    fn name(&self) -> &str {
        "stock_price"
    }

    fn description(&self) -> &str {
        "Look up the current trading price of a stock ticker."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ticker": { "type": "string", "description": "e.g. ACME" }
            },
            "required": ["ticker"],
        })
    }

    /// Reading a price is not dangerous, so it runs without asking.
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    /// The bucket the kernel gates and audits this under. Declaring it
    /// honestly is what keeps the permission model meaningful.
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::NetworkAccess
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let Some(ticker) = call.args["ticker"].as_str() else {
            return ToolOutput::err(&call.id, "ticker is required");
        };
        // A real implementation would call your price service here.
        ToolOutput::ok(&call.id, format!("{ticker} is trading at 42.00 USD"))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = Engine::builder()
        .config(sven_sdk::config::load(None)?)
        .tool(Arc::new(StockPrice))
        .build()?;

    let mut agent = engine.agent("agent");
    let reply = agent
        .send("What is ACME trading at, and is that above 40?")
        .await?
        .reply;

    println!("{reply}");
    Ok(())
}
