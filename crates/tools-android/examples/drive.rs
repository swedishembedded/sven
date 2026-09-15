// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Manual driver for `AndroidTool`, for live exploration/demos against a real
//! device - not a test. Usage:
//!
//!   cargo run -p sven-tools-android --example drive -- <action> '<json args>'
//!
//! e.g.:
//!   cargo run -p sven-tools-android --example drive -- go_home '{}'
//!   cargo run -p sven-tools-android --example drive -- tap '{"x":0.5,"y":0.5}'

use sven_tool_api::tool::{Tool, ToolCall};
use sven_tools_android::AndroidTool;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let action = match args.next() {
        Some(a) => a,
        None => {
            eprintln!("usage: drive <action> '<json args>'");
            std::process::exit(2);
        }
    };
    let raw_args = args.next().unwrap_or_else(|| "{}".to_string());
    let mut parsed: serde_json::Value = match serde_json::from_str(&raw_args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("invalid JSON args: {e}");
            std::process::exit(2);
        }
    };
    parsed
        .as_object_mut()
        .expect("args must be a JSON object")
        .insert("action".to_string(), serde_json::Value::String(action));

    let tool = AndroidTool::default();
    let out = tool
        .execute(&ToolCall {
            id: "drive".into(),
            name: "android".into(),
            args: parsed,
        })
        .await;

    println!("{}", out.content);
    if out.is_error {
        std::process::exit(1);
    }
}
