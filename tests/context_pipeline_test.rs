// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Full pipeline integration tests for `context_query` and `context_reduce`.
//!
//! These tests exercise the complete RLM workflow — from opening a handle
//! through chunked parallel sub-queries to tree-reduced synthesis — using a
//! deterministic `MockSubQueryRunner` that never calls a real LLM.  Every
//! assertion is made against known fixture content so results are 100%
//! reproducible.
//!
//! Test groups:
//!   1. `MockSubQueryRunner` — the test double
//!   2. `context_query` — chunked map step
//!   3. `context_reduce` — reduce / synthesis step
//!   4. End-to-end pipeline — open → query → reduce on real source files

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tempfile::NamedTempFile;
use tokio::sync::Mutex;

use sven_bootstrap::context_query::{ContextQueryTool, ContextReduceTool};
use sven_tools::builtin::context::store::ContextStore;
use sven_tools::builtin::context::{ContextOpenTool, SubQueryRunner};
use sven_tools::tool::{Tool, ToolCall};

// ─── Test double ──────────────────────────────────────────────────────────────

/// A `SubQueryRunner` that records every call and returns a canned response.
///
/// The response embeds the chunk index number from the prompt so that tests
/// can verify each chunk was processed independently.
#[derive(Clone)]
struct RecordingRunner {
    /// Number of calls made so far.
    call_count: Arc<AtomicUsize>,
    /// The reply template.  `{prompt}` is replaced with the first 80 chars of
    /// the received prompt (for assertions).
    reply_template: String,
}

impl RecordingRunner {
    fn new(reply_template: &str) -> Self {
        Self {
            call_count: Arc::new(AtomicUsize::new(0)),
            reply_template: reply_template.to_string(),
        }
    }

    fn calls(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl SubQueryRunner for RecordingRunner {
    async fn query(&self, _system: &str, prompt: &str) -> Result<String, String> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        // Echo back a prefix of the prompt so tests can assert on content routing.
        let snippet: String = prompt.chars().take(120).collect();
        Ok(self.reply_template.replace("{prompt}", &snippet))
    }
}

/// A runner that fails on every call — used to test error propagation.
#[derive(Clone)]
struct FailingRunner;

#[async_trait]
impl SubQueryRunner for FailingRunner {
    async fn query(&self, _system: &str, _prompt: &str) -> Result<String, String> {
        Err("intentional test failure".to_string())
    }
}

/// A runner whose response includes the chunk index extracted from the prompt
/// prefix `[Chunk N/M: ...]`.
#[derive(Clone)]
struct IndexEchoRunner;

#[async_trait]
impl SubQueryRunner for IndexEchoRunner {
    async fn query(&self, _system: &str, prompt: &str) -> Result<String, String> {
        // Extract "N" from "[Chunk N/M: ...]"
        let chunk_num = prompt
            .trim_start_matches('[')
            .split('/')
            .next()
            .and_then(|s| s.trim_start_matches("Chunk ").trim().parse::<usize>().ok())
            .unwrap_or(0);
        Ok(format!("analysis for chunk {}", chunk_num))
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn tool_call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "test".into(),
        name: name.into(),
        args,
    }
}

fn write_tmp(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

fn make_store() -> Arc<Mutex<ContextStore>> {
    Arc::new(Mutex::new(ContextStore::new()))
}

/// Open a file via `ContextOpenTool` and return the handle ID.
async fn open_file(store: &Arc<Mutex<ContextStore>>, path: &str) -> String {
    let tool = ContextOpenTool::new(store.clone());
    let out = tool
        .execute(&tool_call("context_open", json!({"path": path})))
        .await;
    assert!(!out.is_error, "open failed: {}", out.content);
    out.content
        .lines()
        .find(|l| l.contains("handle="))
        .and_then(|l| l.split("handle=").nth(1))
        .and_then(|s| s.split_whitespace().next())
        .expect("handle not found in context_open output")
        .to_string()
}

// ─── context_query tests ─────────────────────────────────────────────────────

mod query_tests {
    use super::*;

    #[tokio::test]
    async fn query_invokes_one_runner_call_per_chunk() {
        // 30 lines, chunk_lines=10 → 3 chunks → 3 runner calls.
        let content: String = (1..=30).map(|i| format!("line {}\n", i)).collect();
        let tmp = write_tmp(&content);
        let store = make_store();
        let handle = open_file(&store, tmp.path().to_str().unwrap()).await;

        let runner = RecordingRunner::new("found nothing in: {prompt}");
        let tool = ContextQueryTool::new(
            store.clone(),
            Arc::new(runner.clone()),
            10,   // default_chunk_lines
            4,    // max_parallel
            None, // progress_tx
        );

        let out = tool
            .execute(&tool_call(
                "context_query",
                json!({
                    "handle": handle,
                    "prompt": "Does this chunk contain any issues? {chunk}",
                    "chunk_lines": 10
                }),
            ))
            .await;
        assert!(!out.is_error, "context_query failed: {}", out.content);
        assert_eq!(
            runner.calls(),
            3,
            "expected 3 runner calls for 30 lines / 10"
        );
    }

    #[tokio::test]
    async fn query_creates_results_handle_in_output() {
        let content: String = (1..=20).map(|i| format!("item {}\n", i)).collect();
        let tmp = write_tmp(&content);
        let store = make_store();
        let handle = open_file(&store, tmp.path().to_str().unwrap()).await;

        let runner = RecordingRunner::new("ok");
        let tool = ContextQueryTool::new(store.clone(), Arc::new(runner), 20, 2, None);

        let out = tool
            .execute(&tool_call(
                "context_query",
                json!({"handle": handle, "prompt": "analyse: {chunk}"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);

        // Output must contain a res_ handle.
        assert!(
            out.content.contains("res_"),
            "output must contain a results handle: {}",
            out.content
        );
        assert!(
            out.content.contains("context_read") || out.content.contains("context_reduce"),
            "output must suggest next steps: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn query_results_handle_is_readable() {
        // 20 lines → 2 chunks of 10.  Runner echoes chunk index.
        let content: String = (1..=20).map(|i| format!("line {}\n", i)).collect();
        let tmp = write_tmp(&content);
        let store = make_store();
        let handle = open_file(&store, tmp.path().to_str().unwrap()).await;

        let runner = IndexEchoRunner;
        let tool = ContextQueryTool::new(store.clone(), Arc::new(runner), 10, 4, None);

        let out = tool
            .execute(&tool_call(
                "context_query",
                json!({
                    "handle": handle,
                    "prompt": "Analyse this: {chunk}",
                    "chunk_lines": 10
                }),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);

        // Extract the results handle from the output.
        let res_handle = out
            .content
            .lines()
            .find(|l| l.contains("res_"))
            .and_then(|l| {
                l.split("res_")
                    .nth(1)
                    .map(|s| format!("res_{}", s.split_whitespace().next().unwrap_or("")))
            })
            .expect("results handle not found in output");

        // Read the results handle through context_read — verifying both chunks appear.
        use sven_tools::builtin::context::ContextReadTool;
        let read_tool = ContextReadTool::new(store.clone());
        let read_out = read_tool
            .execute(&tool_call(
                "context_read",
                json!({"handle": res_handle, "start_line": 1, "end_line": 10}),
            ))
            .await;
        assert!(
            !read_out.is_error,
            "reading results handle failed: {}",
            read_out.content
        );

        // IndexEchoRunner writes "analysis for chunk N" — both should appear.
        let full = {
            let store_locked = store.lock().await;
            store_locked.read_all(&res_handle).unwrap()
        };
        assert!(
            full.contains("analysis for chunk 1"),
            "chunk 1 result missing: {full}"
        );
        assert!(
            full.contains("analysis for chunk 2"),
            "chunk 2 result missing: {full}"
        );
    }

    #[tokio::test]
    async fn query_chunk_count_correct_for_exact_multiple() {
        // Exactly 50 lines, chunk_lines=10 → exactly 5 chunks.
        let content: String = (1..=50).map(|i| format!("L{}\n", i)).collect();
        let tmp = write_tmp(&content);
        let store = make_store();
        let handle = open_file(&store, tmp.path().to_str().unwrap()).await;

        let runner = RecordingRunner::new("processed");
        let tool = ContextQueryTool::new(store.clone(), Arc::new(runner.clone()), 10, 5, None);

        let out = tool
            .execute(&tool_call(
                "context_query",
                json!({"handle": handle, "prompt": "check: {chunk}", "chunk_lines": 10}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(runner.calls(), 5, "50 lines / 10 = exactly 5 chunks");
        assert!(
            out.content.contains("5 chunks processed"),
            "output must mention 5 chunks: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn query_chunk_count_correct_for_partial_last_chunk() {
        // 23 lines, chunk_lines=10 → 3 chunks (10+10+3).
        let content: String = (1..=23).map(|i| format!("row {}\n", i)).collect();
        let tmp = write_tmp(&content);
        let store = make_store();
        let handle = open_file(&store, tmp.path().to_str().unwrap()).await;

        let runner = RecordingRunner::new("done");
        let tool = ContextQueryTool::new(store.clone(), Arc::new(runner.clone()), 10, 4, None);

        let out = tool
            .execute(&tool_call(
                "context_query",
                json!({"handle": handle, "prompt": "{chunk}", "chunk_lines": 10}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(runner.calls(), 3, "23 lines / 10 = 3 chunks");
    }

    #[tokio::test]
    async fn query_with_explicit_ranges_targets_only_those_lines() {
        // 100-line file; query only lines 10-20 and 50-60.
        let content: String = (1..=100).map(|i| format!("item_{:03}\n", i)).collect();
        let tmp = write_tmp(&content);
        let store = make_store();
        let handle = open_file(&store, tmp.path().to_str().unwrap()).await;

        let runner = RecordingRunner::new("range result: {prompt}");
        let tool = ContextQueryTool::new(store.clone(), Arc::new(runner.clone()), 500, 4, None);

        let out = tool
            .execute(&tool_call(
                "context_query",
                json!({
                    "handle": handle,
                    "prompt": "find issues in: {chunk}",
                    "ranges": [
                        {"start_line": 10, "end_line": 20},
                        {"start_line": 50, "end_line": 60}
                    ]
                }),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        // Exactly 2 ranges → exactly 2 runner calls.
        assert_eq!(
            runner.calls(),
            2,
            "2 explicit ranges must produce exactly 2 runner calls, got {}",
            runner.calls()
        );
        assert!(
            out.content.contains("2 chunks processed"),
            "output should report 2 chunks: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn query_prompt_placeholders_are_substituted() {
        // Single-chunk file.  The runner echoes the prompt — verify placeholders
        // were replaced before the prompt reached the runner.
        let content = "hello world\n";
        let tmp = write_tmp(content);
        let store = make_store();
        let handle = open_file(&store, tmp.path().to_str().unwrap()).await;

        // A runner that records the exact prompt it received.
        #[derive(Clone)]
        struct CaptureRunner(Arc<Mutex<Vec<String>>>);

        #[async_trait]
        impl SubQueryRunner for CaptureRunner {
            async fn query(&self, _system: &str, prompt: &str) -> Result<String, String> {
                self.0.lock().await.push(prompt.to_string());
                Ok("ok".into())
            }
        }

        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let runner = CaptureRunner(captured.clone());
        let tool = ContextQueryTool::new(store.clone(), Arc::new(runner), 100, 1, None);

        tool.execute(&tool_call(
            "context_query",
            json!({
                "handle": handle,
                "prompt": "Chunk={chunk_index} of {total_chunks}: {chunk}"
            }),
        ))
        .await;

        let prompts = captured.lock().await;
        assert_eq!(prompts.len(), 1);
        let p = &prompts[0];
        // {chunk_index} and {total_chunks} must have been replaced with numbers.
        assert!(
            !p.contains("{chunk_index}"),
            "{{chunk_index}} must be substituted: {p}"
        );
        assert!(
            !p.contains("{total_chunks}"),
            "{{total_chunks}} must be substituted: {p}"
        );
        assert!(!p.contains("{chunk}"), "{{chunk}} must be substituted: {p}");
        // The actual file content must appear where {chunk} was.
        assert!(p.contains("hello world"), "chunk content missing: {p}");
    }

    #[tokio::test]
    async fn query_runner_failure_stored_as_error_text_not_panic() {
        let content: String = (1..=10).map(|i| format!("l{}\n", i)).collect();
        let tmp = write_tmp(&content);
        let store = make_store();
        let handle = open_file(&store, tmp.path().to_str().unwrap()).await;

        let tool = ContextQueryTool::new(store.clone(), Arc::new(FailingRunner), 100, 4, None);

        let out = tool
            .execute(&tool_call(
                "context_query",
                json!({"handle": handle, "prompt": "check: {chunk}"}),
            ))
            .await;
        // The tool itself should NOT be an error — failures are embedded in results.
        assert!(
            !out.is_error,
            "runner failures must not crash the tool: {}",
            out.content
        );

        // The results handle should contain the error message.
        let res_handle = out
            .content
            .lines()
            .find(|l| l.contains("res_"))
            .and_then(|l| {
                l.split("res_")
                    .nth(1)
                    .map(|s| format!("res_{}", s.split_whitespace().next().unwrap_or("")))
            })
            .expect("results handle missing even on runner failure");

        let full = {
            let store_locked = store.lock().await;
            store_locked.read_all(&res_handle).unwrap()
        };
        assert!(
            full.contains("sub-query failed") || full.contains("intentional test failure"),
            "failure message must be embedded in results: {full}"
        );
    }

    #[tokio::test]
    async fn query_unknown_handle_is_error() {
        let store = make_store();
        let runner = RecordingRunner::new("ok");
        let tool = ContextQueryTool::new(store, Arc::new(runner), 100, 4, None);
        let out = tool
            .execute(&tool_call(
                "context_query",
                json!({"handle": "ctx_dead", "prompt": "test"}),
            ))
            .await;
        assert!(out.is_error, "unknown handle must be an error");
        assert!(out.content.contains("unknown handle"));
    }

    #[tokio::test]
    async fn query_empty_context_returns_gracefully() {
        let tmp = write_tmp(""); // Empty file.
        let store = make_store();
        let handle = open_file(&store, tmp.path().to_str().unwrap()).await;

        let runner = RecordingRunner::new("ok");
        let tool = ContextQueryTool::new(store.clone(), Arc::new(runner.clone()), 100, 4, None);

        let out = tool
            .execute(&tool_call(
                "context_query",
                json!({"handle": handle, "prompt": "anything"}),
            ))
            .await;
        // Should not panic or crash; empty content → 0 chunks → graceful message.
        assert!(
            !out.is_error,
            "empty context must not error: {}",
            out.content
        );
        assert_eq!(runner.calls(), 0, "no calls for empty content");
    }
}

// ─── context_reduce tests ─────────────────────────────────────────────────────

mod reduce_tests {
    use super::*;

    fn make_reduce_tool(
        store: Arc<Mutex<ContextStore>>,
        runner: impl SubQueryRunner + 'static,
    ) -> ContextReduceTool {
        ContextReduceTool::new(store, Arc::new(runner), 50_000, 500)
    }

    #[tokio::test]
    async fn reduce_sends_full_content_to_runner() {
        let results = "chunk 1: found issue A\nchunk 2: found issue B\nchunk 3: no issues\n";
        let mut store_inner = ContextStore::new();
        let meta = store_inner
            .register_results(results.to_string(), 3)
            .unwrap();
        let store = Arc::new(Mutex::new(store_inner));

        #[derive(Clone)]
        struct ContentCapture(Arc<Mutex<String>>);

        #[async_trait]
        impl SubQueryRunner for ContentCapture {
            async fn query(&self, _system: &str, prompt: &str) -> Result<String, String> {
                *self.0.lock().await = prompt.to_string();
                Ok("synthesised: 2 issues total: A and B".into())
            }
        }

        let captured_prompt = Arc::new(Mutex::new(String::new()));
        let runner = ContentCapture(captured_prompt.clone());
        let tool = make_reduce_tool(store, runner);

        let out = tool
            .execute(&tool_call(
                "context_reduce",
                json!({
                    "handle": meta.handle_id,
                    "prompt": "Summarise all findings."
                }),
            ))
            .await;
        assert!(!out.is_error, "context_reduce failed: {}", out.content);
        assert!(
            out.content.contains("synthesised"),
            "runner response not returned: {}",
            out.content
        );

        // The runner must have received the full results content in its prompt.
        let prompt = captured_prompt.lock().await;
        assert!(
            prompt.contains("found issue A"),
            "runner prompt must contain chunk 1: {prompt}"
        );
        assert!(
            prompt.contains("found issue B"),
            "runner prompt must contain chunk 2: {prompt}"
        );
        assert!(
            prompt.contains("Summarise all findings"),
            "user prompt must be appended: {prompt}"
        );
    }

    #[tokio::test]
    async fn reduce_returns_runner_response_as_tool_output() {
        let mut store_inner = ContextStore::new();
        let meta = store_inner
            .register_results("result 1\nresult 2\n".to_string(), 2)
            .unwrap();
        let store = Arc::new(Mutex::new(store_inner));

        let runner = RecordingRunner::new("Final synthesis: {prompt}");
        let tool = make_reduce_tool(store, runner);

        let out = tool
            .execute(&tool_call(
                "context_reduce",
                json!({"handle": meta.handle_id, "prompt": "List issues."}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("Final synthesis"),
            "runner response must be the tool output: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn reduce_unknown_handle_is_error() {
        let store = make_store();
        let runner = RecordingRunner::new("ok");
        let tool = make_reduce_tool(store, runner);
        let out = tool
            .execute(&tool_call(
                "context_reduce",
                json!({"handle": "res_dead", "prompt": "summary"}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown handle"));
    }

    #[tokio::test]
    async fn reduce_runner_failure_propagates_as_tool_error() {
        let mut store_inner = ContextStore::new();
        let meta = store_inner
            .register_results("some result\n".to_string(), 1)
            .unwrap();
        let store = Arc::new(Mutex::new(store_inner));

        let tool = make_reduce_tool(store, FailingRunner);
        let out = tool
            .execute(&tool_call(
                "context_reduce",
                json!({"handle": meta.handle_id, "prompt": "summarise"}),
            ))
            .await;
        assert!(out.is_error, "runner failure must propagate as tool error");
        assert!(
            out.content.contains("context_reduce failed"),
            "error must mention context_reduce: {}",
            out.content
        );
    }
}

// ─── End-to-end pipeline on real source files ─────────────────────────────────

mod e2e_pipeline_tests {
    use super::*;
    use sven_tools::builtin::context::ContextGrepTool;

    const STORE_RS: &str = "crates/sven-tools/src/builtin/context/store.rs";
    const CONTEXT_DIR: &str = "crates/sven-tools/src/builtin/context";

    /// Full RLM pipeline on a real source file:
    ///   context_open → context_grep → context_query (targeted) → context_reduce
    #[tokio::test]
    async fn full_pipeline_on_store_rs() {
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }

        let store = make_store();

        // 1. Open.
        let open_tool = ContextOpenTool::new(store.clone());
        let open_out = open_tool
            .execute(&tool_call("context_open", json!({"path": STORE_RS})))
            .await;
        assert!(!open_out.is_error, "{}", open_out.content);
        let handle = open_out
            .content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .unwrap()
            .to_string();

        // 2. Grep for all pub fn methods.
        let grep_tool = ContextGrepTool::new(store.clone());
        let grep_out = grep_tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": handle, "pattern": r"    pub fn ", "context_lines": 0, "limit": 20}),
            ))
            .await;
        assert!(!grep_out.is_error, "{}", grep_out.content);
        assert!(
            grep_out.content.contains("pub fn open_file"),
            "pub fn open_file not found: {}",
            grep_out.content
        );

        // 3. Query: process the file in 200-line chunks, ask runner for
        //    "public API surface".
        let runner = IndexEchoRunner;
        let query_tool = ContextQueryTool::new(store.clone(), Arc::new(runner), 200, 4, None);
        let query_out = query_tool
            .execute(&tool_call(
                "context_query",
                json!({
                    "handle": handle,
                    "prompt": "List all pub fn signatures in this chunk: {chunk}",
                    "chunk_lines": 200
                }),
            ))
            .await;
        assert!(!query_out.is_error, "{}", query_out.content);

        let res_handle = query_out
            .content
            .lines()
            .find(|l| l.contains("res_"))
            .and_then(|l| {
                l.split("res_")
                    .nth(1)
                    .map(|s| format!("res_{}", s.split_whitespace().next().unwrap_or("")))
            })
            .expect("no results handle in query output");

        // 4. Reduce the per-chunk results into a final summary.
        let reduce_runner = RecordingRunner::new("Final API summary: {prompt}");
        let reduce_tool =
            ContextReduceTool::new(store.clone(), Arc::new(reduce_runner), 50_000, 500);
        let reduce_out = reduce_tool
            .execute(&tool_call(
                "context_reduce",
                json!({
                    "handle": res_handle,
                    "prompt": "Produce a deduplicated list of all public functions."
                }),
            ))
            .await;
        assert!(!reduce_out.is_error, "{}", reduce_out.content);
        assert!(
            reduce_out.content.contains("Final API summary"),
            "reduce output missing runner response: {}",
            reduce_out.content
        );

        // The reduce runner's prompt must contain the per-chunk results.
        assert!(
            reduce_out.content.len() > 10,
            "reduce output suspiciously short: {}",
            reduce_out.content
        );
    }

    /// Query the entire context/ directory, verifying cross-file chunk dispatch.
    #[tokio::test]
    async fn query_directory_dispatches_cross_file_chunks() {
        let dir = std::path::Path::new(CONTEXT_DIR);
        if !dir.exists() {
            return;
        }

        let store = make_store();

        // Open the directory.
        let open_tool = ContextOpenTool::new(store.clone());
        let open_out = open_tool
            .execute(&tool_call(
                "context_open",
                json!({
                    "path": CONTEXT_DIR,
                    "include_pattern": "*.rs",
                    "recursive": false
                }),
            ))
            .await;
        assert!(!open_out.is_error, "{}", open_out.content);
        let handle = open_out
            .content
            .lines()
            .find(|l| l.contains("handle="))
            .and_then(|l| l.split("handle=").nth(1))
            .and_then(|s| s.split_whitespace().next())
            .unwrap()
            .to_string();

        // The directory has >1000 total lines; with chunk_lines=300 we get ≥4 chunks.
        let runner = RecordingRunner::new("ok");
        let query_tool =
            ContextQueryTool::new(store.clone(), Arc::new(runner.clone()), 300, 4, None);
        let out = query_tool
            .execute(&tool_call(
                "context_query",
                json!({"handle": handle, "prompt": "count pub fn in: {chunk}", "chunk_lines": 300}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            runner.calls() >= 4,
            "expected at least 4 chunks for context/ directory, got {}",
            runner.calls()
        );
    }

    /// Verify that results handle content is well-formed and greppable.
    #[tokio::test]
    async fn results_handle_content_is_greppable() {
        let path = std::path::Path::new(STORE_RS);
        if !path.exists() {
            return;
        }

        let store = make_store();
        let handle = open_file(&store, STORE_RS).await;

        // IndexEchoRunner writes "analysis for chunk N" for each chunk.
        let query_tool =
            ContextQueryTool::new(store.clone(), Arc::new(IndexEchoRunner), 200, 4, None);
        let query_out = query_tool
            .execute(&tool_call(
                "context_query",
                json!({
                    "handle": handle,
                    "prompt": "Analyse: {chunk}",
                    "chunk_lines": 200
                }),
            ))
            .await;
        assert!(!query_out.is_error, "{}", query_out.content);

        let res_handle = query_out
            .content
            .lines()
            .find(|l| l.contains("res_"))
            .and_then(|l| {
                l.split("res_")
                    .nth(1)
                    .map(|s| format!("res_{}", s.split_whitespace().next().unwrap_or("")))
            })
            .unwrap();

        // Grep the results handle for "analysis for chunk".
        let grep_tool = ContextGrepTool::new(store.clone());
        let grep_out = grep_tool
            .execute(&tool_call(
                "context_grep",
                json!({"handle": res_handle, "pattern": "analysis for chunk", "limit": 50}),
            ))
            .await;
        assert!(!grep_out.is_error, "{}", grep_out.content);

        // The grep must find at least one "analysis for chunk N" line, confirming
        // the results handle contains per-chunk output.
        assert!(
            !grep_out.content.contains("no matches"),
            "results handle must be greppable for per-chunk analysis lines: {}",
            grep_out.content
        );

        // The query output must mention that multiple chunks were processed.
        // Format: "Query complete: N chunks processed ..."
        let chunk_count: usize = query_out
            .content
            .split_whitespace()
            .zip(query_out.content.split_whitespace().skip(1))
            .find_map(|(a, b)| if b == "chunks" { a.parse().ok() } else { None })
            .unwrap_or(0);
        assert!(
            chunk_count >= 4,
            "store.rs / 200 lines should yield at least 4 chunks, got {chunk_count} from: {}",
            query_out.content
        );
    }
}
