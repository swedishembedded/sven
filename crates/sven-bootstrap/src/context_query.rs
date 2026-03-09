// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `context_query` and `context_reduce` tools plus the `ModelSubQueryRunner`
//! implementation.
//!
//! These tools implement the map and reduce steps of the RLM (Recursive
//! Language Model) pattern described in arxiv.org/abs/2512.24601.
//!
//! `context_query` — dispatches a prompt to sub-agents over chunks of a
//! memory-mapped context handle, storing results as a new handle.
//!
//! `context_reduce` — synthesizes/aggregates a results handle using a sub-
//! agent, with automatic tree reduction when results exceed the sub-query
//! context window.
//!
//! `ModelSubQueryRunner` — concrete implementation of the `SubQueryRunner`
//! trait backed by `sven_model::ModelProvider::complete`.  Each call sends a
//! minimal `CompletionRequest` with no tools — matching the paper's
//! `llm_query()` helper.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use sven_config::{AgentMode, Config};
use sven_model::{CompletionRequest, Message, ModelProvider, ResponseEvent};
use sven_tools::{
    events::ToolEvent,
    policy::ApprovalPolicy,
    tool::{OutputCategory, Tool, ToolCall, ToolOutput},
    ContextStore, SubQueryRunner,
};

// ─── ModelSubQueryRunner ──────────────────────────────────────────────────────

/// Wraps a `ModelProvider` to implement `SubQueryRunner`.
///
/// Each `query` call sends a two-message `CompletionRequest` (system + user)
/// with **no tools** and collects the full streamed text response.  This
/// matches the paper's `llm_query()` which is a plain completion, not an agent.
pub struct ModelSubQueryRunner {
    provider: Arc<dyn ModelProvider>,
    /// Soft cap on the prompt sent to each sub-query.  When the caller's
    /// prompt exceeds this limit it is truncated with a notice.
    max_chars: usize,
    /// Per-call timeout.  Zero means no timeout.
    timeout: Option<Duration>,
}

impl ModelSubQueryRunner {
    pub fn new(provider: Arc<dyn ModelProvider>, max_chars: usize, timeout_secs: u64) -> Self {
        let timeout = if timeout_secs > 0 {
            Some(Duration::from_secs(timeout_secs))
        } else {
            None
        };
        Self {
            provider,
            max_chars,
            timeout,
        }
    }

    async fn query_inner(&self, system: &str, prompt: &str) -> Result<String, String> {
        // Cap the prompt to avoid exceeding the sub-query context window.
        let prompt_str = if prompt.len() > self.max_chars {
            format!(
                "{}\n[... {} bytes omitted — prompt truncated to fit sub-query context ...]",
                &prompt[..self.max_chars],
                prompt.len() - self.max_chars
            )
        } else {
            prompt.to_string()
        };

        let messages = vec![Message::system(system), Message::user(prompt_str)];

        let req = CompletionRequest {
            messages,
            tools: vec![],
            stream: true,
            system_dynamic_suffix: None,
            cache_key: None,
            max_output_tokens_override: None,
        };

        let mut stream = self
            .provider
            .complete(req)
            .await
            .map_err(|e| format!("sub-query provider error: {e}"))?;

        let mut text = String::new();
        while let Some(event) = stream.next().await {
            match event {
                Ok(ResponseEvent::TextDelta(delta)) => text.push_str(&delta),
                // Break on Done or Usage — both signal the end of the response.
                // Usage often arrives last on OpenAI-compatible providers that
                // omit an explicit Done frame.
                Ok(ResponseEvent::Done) | Ok(ResponseEvent::Usage { .. }) => break,
                Ok(ResponseEvent::Error(e)) => return Err(format!("sub-query stream error: {e}")),
                Ok(_) => {}
                Err(e) => return Err(format!("sub-query stream error: {e}")),
            }
        }

        Ok(text)
    }
}

#[async_trait]
impl SubQueryRunner for ModelSubQueryRunner {
    async fn query(&self, system: &str, prompt: &str) -> Result<String, String> {
        match self.timeout {
            Some(dur) => tokio::time::timeout(dur, self.query_inner(system, prompt))
                .await
                .unwrap_or_else(|_| Err(format!("sub-query timed out after {} s", dur.as_secs()))),
            None => self.query_inner(system, prompt).await,
        }
    }
}

// ─── ContextQueryTool ─────────────────────────────────────────────────────────

/// Dispatches analysis to sub-agents over chunks of a context handle.
pub struct ContextQueryTool {
    store: Arc<Mutex<ContextStore>>,
    runner: Arc<dyn SubQueryRunner>,
    default_chunk_lines: usize,
    max_parallel: usize,
    /// Optional channel for emitting real-time progress events to the UI.
    progress_tx: Option<mpsc::Sender<ToolEvent>>,
}

impl ContextQueryTool {
    pub fn new(
        store: Arc<Mutex<ContextStore>>,
        runner: Arc<dyn SubQueryRunner>,
        default_chunk_lines: usize,
        max_parallel: usize,
        progress_tx: Option<mpsc::Sender<ToolEvent>>,
    ) -> Self {
        Self {
            store,
            runner,
            default_chunk_lines,
            max_parallel,
            progress_tx,
        }
    }

    fn emit_progress(&self, call_id: &str, message: String) {
        if let Some(tx) = &self.progress_tx {
            let _ = tx.try_send(ToolEvent::Progress {
                call_id: call_id.to_string(),
                message,
            });
        }
    }
}

#[async_trait]
impl Tool for ContextQueryTool {
    fn name(&self) -> &str {
        "context_query"
    }

    fn description(&self) -> &str {
        "Dispatch analysis to sub-agents over chunks of a memory-mapped context. Each sub-agent \
         receives one chunk of content plus your prompt, processes it independently, and returns \
         a result. This is the map step of a map-reduce pattern — use context_reduce for the \
         reduce step.\n\n\
         Results are stored as a new context handle (not loaded into your context window). \
         Use context_read to inspect specific results, context_grep to search them, or \
         context_reduce to synthesize a final answer.\n\n\
         Strategies:\n\
         - Full scan: omit ranges to chunk and analyze the entire context\n\
         - Targeted scan: use context_grep first, then specify ranges to query only matched sections\n\
         - Iterative refinement: query, inspect results, query again on specific chunks\n\n\
         The sub-agents receive ONLY the chunk and your prompt. They do not have access to your \
         conversation history, tools, or other context. Design your prompt to be self-contained.\n\n\
         Example prompt: \"Analyze this C code for memory safety issues. For each issue found, \
         report the line number, severity (high/medium/low), and a one-line description:\\n\\n{chunk}\""
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "string",
                    "description": "Context handle ID returned by context_open"
                },
                "prompt": {
                    "type": "string",
                    "description": "Analysis prompt sent to each sub-agent. Use {chunk} as a placeholder \
                                    for the chunk content, {chunk_index} for the 0-based index, \
                                    and {total_chunks} for the total number of chunks."
                },
                "ranges": {
                    "type": "array",
                    "description": "Optional list of line ranges to query. Omit to process the entire \
                                    context. Each entry: {start_line, end_line, file (optional)}.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "start_line": {"type": "integer"},
                            "end_line": {"type": "integer"},
                            "file": {"type": "string"}
                        },
                        "required": ["start_line", "end_line"]
                    }
                },
                "chunk_lines": {
                    "type": "integer",
                    "description": "Lines per chunk (default: configured default_chunk_lines). \
                                    Ignored when ranges are specified — each range is one chunk."
                },
                "max_parallel": {
                    "type": "integer",
                    "description": "Maximum concurrent sub-agent queries (default: configured max_parallel)"
                }
            },
            "required": ["handle", "prompt"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn output_category(&self) -> OutputCategory {
        OutputCategory::Generic
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) => h.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'handle'"),
        };
        let prompt_template = match call.args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'prompt'"),
        };
        let chunk_lines = call
            .args
            .get("chunk_lines")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(self.default_chunk_lines);
        let max_parallel = call
            .args
            .get("max_parallel")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(self.max_parallel);

        debug!(handle = %handle, "context_query tool");
        self.emit_progress(
            &call.id,
            format!("context_query: preparing chunks for {handle}"),
        );

        // ── Build list of (chunk_index, total, label, content) tuples ─────────
        let chunks: Vec<(usize, usize, String, String)> = {
            let store = self.store.lock().await;

            if !store.contains(&handle) {
                return ToolOutput::err(
                    &call.id,
                    format!("unknown handle '{}'. Use context_open first.", handle),
                );
            }

            // Check for explicit ranges.
            if let Some(ranges_val) = call.args.get("ranges").and_then(|v| v.as_array()) {
                let total = ranges_val.len();
                let mut out = Vec::with_capacity(total);
                for (i, range) in ranges_val.iter().enumerate() {
                    let start = range
                        .get("start_line")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(1) as usize;
                    let end = range
                        .get("end_line")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(start as u64) as usize;
                    let file = range
                        .get("file")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let text = match store.read_range(&handle, start, end, file.as_deref()) {
                        Ok(t) => t,
                        Err(e) => {
                            return ToolOutput::err(
                                &call.id,
                                format!("error reading range {}-{}: {}", start, end, e),
                            )
                        }
                    };
                    let label = format!("L{}-L{}", start, end);
                    out.push((i, total, label, text));
                }
                out
            } else {
                // Auto-chunk the entire context.
                let mut out = Vec::new();
                if let Err(e) = store.chunks(&handle, chunk_lines, |idx, total, label, text| {
                    out.push((idx, total, label.to_string(), text.to_string()));
                    Ok(())
                }) {
                    return ToolOutput::err(&call.id, format!("chunking error: {e}"));
                }
                out
            }
        };

        if chunks.is_empty() {
            return ToolOutput::ok(&call.id, "No content to query (empty context).");
        }

        let total_chunks = chunks.len();
        info!(total_chunks, handle = %handle, "context_query: starting sub-queries");
        self.emit_progress(
            &call.id,
            format!("context_query: {total_chunks} chunks, {max_parallel} parallel — starting"),
        );

        // ── Dispatch sub-queries in batches of max_parallel ──────────────────
        let runner = self.runner.clone();
        let mut results: Vec<(usize, String)> = Vec::with_capacity(total_chunks);
        let mut completed = 0usize;

        const SUB_QUERY_SYSTEM: &str =
            "You are a focused analysis sub-agent. Answer the question or perform the analysis \
             described in the user prompt using only the provided content. Be concise and \
             structured. Do not ask for clarification.";

        for batch in chunks.chunks(max_parallel) {
            let mut handles: Vec<tokio::task::JoinHandle<(usize, Result<String, String>)>> =
                Vec::with_capacity(batch.len());

            for (idx, total, label, text) in batch {
                let prompt = prompt_template
                    .replace("{chunk}", text)
                    .replace("{chunk_index}", &idx.to_string())
                    .replace("{total_chunks}", &total.to_string());
                let full_prompt = format!("[Chunk {}/{}: {}]\n\n{}", idx + 1, total, label, prompt);
                let runner_clone = runner.clone();
                let idx_copy = *idx;
                handles.push(tokio::spawn(async move {
                    let res = runner_clone.query(SUB_QUERY_SYSTEM, &full_prompt).await;
                    (idx_copy, res)
                }));
            }

            for handle in handles {
                match handle.await {
                    Ok((idx, Ok(text))) => results.push((idx, text)),
                    Ok((idx, Err(e))) => {
                        warn!(chunk = idx, error = %e, "sub-query failed");
                        results.push((idx, format!("[chunk {} sub-query failed: {}]", idx, e)));
                    }
                    Err(e) => {
                        warn!(error = %e, "sub-query task panicked");
                    }
                }
                completed += 1;
                info!(completed, total_chunks, "context_query: chunk processed");
                self.emit_progress(
                    &call.id,
                    format!("context_query: chunk {completed}/{total_chunks}"),
                );
            }
        }

        // Sort results by chunk index to maintain order.
        results.sort_by_key(|(idx, _)| *idx);

        // ── Register results in memory as a new handle ────────────────────────
        let results_text: String = results
            .iter()
            .map(|(idx, text)| format!("=== chunk {} of {} ===\n{}\n", idx + 1, total_chunks, text))
            .collect::<Vec<_>>()
            .join("\n");

        let meta = {
            let mut store = self.store.lock().await;
            match store.register_results(results_text, total_chunks) {
                Ok(m) => m,
                Err(e) => {
                    return ToolOutput::err(&call.id, format!("failed to register results: {e}"))
                }
            }
        };

        // Show a preview of the first result.
        let preview = results
            .first()
            .map(|(_, t)| {
                let first_line = t
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect::<String>();
                format!("\nPreview of chunk 1 result:\n  {}", first_line)
            })
            .unwrap_or_default();

        ToolOutput::ok(
            &call.id,
            format!(
                "Query complete: {} chunks processed ({} parallel batches).\n\
                 Results stored in handle: {} ({} bytes, {} entries){}\n\n\
                 Use context_read(handle=\"{}\", start_line=N, end_line=M) to inspect results.\n\
                 Use context_grep(handle=\"{}\", pattern=\"...\") to search results.\n\
                 Use context_reduce(handle=\"{}\", prompt=\"...\") to synthesize a final answer.",
                total_chunks,
                total_chunks.div_ceil(max_parallel),
                meta.handle_id,
                meta.total_bytes,
                total_chunks,
                preview,
                meta.handle_id,
                meta.handle_id,
                meta.handle_id,
            ),
        )
    }
}

// ─── ContextReduceTool ────────────────────────────────────────────────────────

/// Synthesizes results from a context handle using a sub-agent.
///
/// If the content fits in one sub-query call it is sent directly.  If not, it
/// is chunked and reduced hierarchically (tree reduction): each chunk produces
/// an intermediate result, and those intermediate results are then reduced
/// again until a single final answer is produced.
pub struct ContextReduceTool {
    store: Arc<Mutex<ContextStore>>,
    runner: Arc<dyn SubQueryRunner>,
    max_chars: usize,
    chunk_lines: usize,
}

impl ContextReduceTool {
    pub fn new(
        store: Arc<Mutex<ContextStore>>,
        runner: Arc<dyn SubQueryRunner>,
        max_chars: usize,
        chunk_lines: usize,
    ) -> Self {
        Self {
            store,
            runner,
            max_chars,
            chunk_lines,
        }
    }
}

#[async_trait]
impl Tool for ContextReduceTool {
    fn name(&self) -> &str {
        "context_reduce"
    }

    fn description(&self) -> &str {
        "Aggregate and synthesize results from a context handle using a sub-agent. This is \
         the reduce step after context_query's map step.\n\n\
         The sub-agent receives all results from the handle plus your prompt. If the results \
         exceed the sub-agent's context window, they are automatically chunked and reduced \
         hierarchically (tree reduction) until a single final answer is produced.\n\n\
         Use this when:\n\
         - context_query returned per-chunk findings that need combining\n\
         - You need to deduplicate, rank, or summarize across chunks\n\
         - The final answer requires reasoning over all chunk results together\n\n\
         For simple cases where you can synthesize from context_read output directly, \
         skip this tool and reason in your own response.\n\n\
         Returns the synthesized answer as direct text."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "string",
                    "description": "Context handle ID containing results to aggregate \
                                    (typically from context_query)"
                },
                "prompt": {
                    "type": "string",
                    "description": "Synthesis prompt. The full handle content is prepended \
                                    automatically before your prompt."
                }
            },
            "required": ["handle", "prompt"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn output_category(&self) -> OutputCategory {
        OutputCategory::Generic
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) => h.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'handle'"),
        };
        let prompt = match call.args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'prompt'"),
        };

        debug!(handle = %handle, "context_reduce tool");

        let content = {
            let store = self.store.lock().await;
            if !store.contains(&handle) {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "unknown handle '{}'. Use context_open or context_query first.",
                        handle
                    ),
                );
            }
            match store.read_all(&handle) {
                Ok(c) => c,
                Err(e) => return ToolOutput::err(&call.id, format!("failed to read handle: {e}")),
            }
        };

        const REDUCE_SYSTEM: &str =
            "You are a synthesis sub-agent. Your task is to aggregate, deduplicate, and \
             summarize the provided findings or results according to the user's instructions. \
             Be concise, structured, and complete. Do not ask for clarification.";

        let result = tree_reduce(
            &*self.runner,
            REDUCE_SYSTEM,
            &content,
            &prompt,
            self.max_chars,
            self.chunk_lines,
        )
        .await;

        match result {
            Ok(text) => ToolOutput::ok(&call.id, text),
            Err(e) => ToolOutput::err(&call.id, format!("context_reduce failed: {e}")),
        }
    }
}

// ─── Tree reduction helper ────────────────────────────────────────────────────

/// Reduce `content` using the runner, applying tree reduction when content
/// exceeds `max_chars`.
///
/// Strategy:
/// 1. If `content.len() <= max_chars`: send directly.
/// 2. Otherwise: split content into `chunk_lines`-line chunks, reduce each
///    chunk independently (collect intermediate results), then recursively
///    reduce the intermediate results.
///
/// This terminates because each reduction level produces less text than its
/// input (the runner is asked to summarize/synthesize, which compresses).
/// A depth guard prevents runaway recursion.
async fn tree_reduce(
    runner: &dyn SubQueryRunner,
    system: &str,
    content: &str,
    prompt: &str,
    max_chars: usize,
    chunk_lines: usize,
) -> Result<String, String> {
    tree_reduce_inner(runner, system, content, prompt, max_chars, chunk_lines, 0).await
}

const MAX_REDUCE_DEPTH: usize = 4;

async fn tree_reduce_inner(
    runner: &dyn SubQueryRunner,
    system: &str,
    content: &str,
    prompt: &str,
    max_chars: usize,
    chunk_lines: usize,
    depth: usize,
) -> Result<String, String> {
    if depth >= MAX_REDUCE_DEPTH {
        // At max depth: just truncate and send.
        let truncated = if content.len() > max_chars {
            format!(
                "{}\n[... {} bytes omitted — tree reduction depth limit reached ...]",
                &content[..max_chars],
                content.len() - max_chars
            )
        } else {
            content.to_string()
        };
        let user_prompt = format!("{}\n\n{}", truncated, prompt);
        return runner.query(system, &user_prompt).await;
    }

    if content.len() <= max_chars {
        // Fits in one call.
        let user_prompt = format!("{}\n\n{}", content, prompt);
        return runner.query(system, &user_prompt).await;
    }

    // Split into line chunks and reduce each one.
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let chunks = total.div_ceil(chunk_lines);
    let chunk_prompt = format!(
        "Summarize and extract the key findings from this section. \
         Preserve all important details, line numbers, and specific values. \
         This is an intermediate step toward: {}",
        prompt
    );

    let mut intermediate_results: Vec<String> = Vec::with_capacity(chunks);
    for chunk_idx in 0..chunks {
        let start = chunk_idx * chunk_lines;
        let end = ((chunk_idx + 1) * chunk_lines).min(total);
        let chunk_text = lines[start..end].join("\n");
        let user_prompt = format!("{}\n\n{}", chunk_text, chunk_prompt);
        let result = runner.query(system, &user_prompt).await?;
        intermediate_results.push(result);
    }

    let combined = intermediate_results.join("\n\n---\n\n");
    Box::pin(tree_reduce_inner(
        runner,
        system,
        &combined,
        prompt,
        max_chars,
        chunk_lines,
        depth + 1,
    ))
    .await
}

// ─── Public constructor helpers ───────────────────────────────────────────────

/// Build both context tools from configuration and a shared provider.
///
/// `progress_tx` is the tool-event channel used to emit real-time chunk progress
/// to the agent loop and then to the UI spinner.  Pass `None` in headless/test
/// contexts where no UI is attached.
///
/// Returns `(ContextQueryTool, ContextReduceTool)`.
pub fn build_context_query_tools(
    store: Arc<Mutex<ContextStore>>,
    provider: Arc<dyn ModelProvider>,
    cfg: &Config,
    progress_tx: Option<mpsc::Sender<ToolEvent>>,
) -> (ContextQueryTool, ContextReduceTool) {
    let runner: Arc<dyn SubQueryRunner> = Arc::new(ModelSubQueryRunner::new(
        provider,
        cfg.tools.context.sub_query_max_chars,
        cfg.tools.context.sub_query_timeout_secs,
    ));

    let query_tool = ContextQueryTool::new(
        store.clone(),
        runner.clone(),
        cfg.tools.context.default_chunk_lines,
        cfg.tools.context.max_parallel,
        progress_tx,
    );

    let reduce_tool = ContextReduceTool::new(
        store,
        runner,
        cfg.tools.context.sub_query_max_chars,
        cfg.tools.context.default_chunk_lines,
    );

    (query_tool, reduce_tool)
}
