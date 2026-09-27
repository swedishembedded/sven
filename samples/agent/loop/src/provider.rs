// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements coding agents that run their model
// in-process - no separately versioned serving process to drift from the
// build. If your team needs expertise in local inference integration or
// provider abstractions, you can procure our services by sending an email
// to info@swedishembedded.com.

//! The local model provider: brain's Qwen3 stack, linked in-process.
//!
//! sven's [`ModelProvider`] seam is what remote providers (OpenAI,
//! Anthropic, OpenRouter) hang off; this module hangs a LOCAL model off the
//! same seam, so the loop agent runs the same engine the wire providers do
//! (same tool loop, same event stream, same usage accounting) with no HTTP
//! hop and no dependency on a separately running, separately versioned
//! `brain serve`: the sample is built against the brain crates it links,
//! and loads weights and adapters directly from disk at startup.
//!
//! The generation path is the one brain's own serving uses for a single
//! sequence: chat-template render (`qwen3::chat`), KV-cached decode
//! (`qwen3::sample`), tool-call scanning (`ChatScanner` via `SeqState`).
//! A trained LoRA adapter is folded into the base tensors before the model
//! is built (`qwen3::lora::fold_adapter_into`), the same fold
//! `qwen3::eval::score_chat` uses, so a served adapter is numerically the
//! model it was trained to be.

use anyhow::Context;
use capability::{CancelToken, Invocation, Outcome, Progress};
use checkpoint::weightio::WeightReader;
use data::qwen_tokenizer::QwenBpe;
use data::rng::Rng;
use data::tokenizer::Tokenizer;
use qwen3::chat::{self, SeqState};
use qwen3::lora;
use qwen3::model::Qwen;
use qwen3::sample::generate_kv_stream_cancellable;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use sven_sdk::model::{
    CompletionRequest, ContentPart, Message, MessageContent, ModelProvider, ResponseEvent, Role,
    ToolSchema,
};

/// Sampling defaults for agent work, applied per request. Low temperature:
/// an agent is executing a procedure, not writing prose; the small models
/// this provider serves drift into repetition well before they drift into
/// creativity at higher temperatures. The generation cap is bounded to what
/// one agentic step needs - a completion that has not concluded within a few
/// hundred tokens is looping, and at this hardware's measured decode rate a
/// larger cap would spend an entire attempt budget on one never-ending
/// generation.
const DEFAULT_MAX_NEW_TOKENS: usize = 512;
const DEFAULT_TEMPERATURE: f64 = 0.2;
const DEFAULT_TOP_K: i64 = 20;

/// Prefill chunk size, in prompt tokens. Prefill runs in chunks of this many
/// tokens so a cancellation lands within one chunk instead of after the
/// whole prompt: at this device's measured prefill rate a 5000-token agent
/// prompt is minutes of one uninterruptible device wait when prefilled in a
/// single call, which is what hung an abandoned timeout run until it
/// finished - and made the process exit under it crash. 512 chunks keep the
/// extra readbacks noise against the per-chunk compute while bounding the
/// cancellation latency to well under half a minute.
const PREFILL_CHUNK_TOKENS: usize = 512;

/// A loaded Qwen3 model (optionally with a folded LoRA adapter) plus its
/// tokenizer, ready to complete. One sequence decodes at a time - the model
/// itself carries the KV cache across a generation - so requests serialize
/// behind a lock; that is the shape of a single-user local agent, not a
/// serving fleet.
pub struct LocalQwen {
    model: Arc<Mutex<Qwen>>,
    head: Arc<Vec<f32>>,
    tok: Arc<QwenBpe>,
    eos: Arc<Vec<u32>>,
    model_name: String,
    max_new_tokens: usize,
    temperature: f64,
    /// Inline context budget: the KV cache is sized for exactly this many
    /// tokens, so a prompt plus its generation must fit inside it.
    context_tokens: u32,
    /// The in-flight generation's cancel token, if one is running. The
    /// runner arms it - through [`Self::stop_generation`] - when the attempt
    /// ends before the turn does, so a decode stops within one prefill chunk
    /// or one token instead of running to its cap.
    in_flight: Arc<Mutex<Option<CancelToken>>>,
    /// Generations currently running. [`Self::stop_generation`] waits on
    /// this reaching zero so the process never exits under a live device
    /// call - exit racing a Vulkan submit is a segfault, not a controlled
    /// outcome.
    live: Arc<AtomicUsize>,
}

/// Where the weights come from and which adapter rides on top.
#[derive(Clone, Debug)]
pub struct LocalWeights {
    /// Checkpoint directory (or file) - the same layout brain's model store
    /// uses. A directory resolves to the checkpoint inside it.
    pub base: std::path::PathBuf,
    /// Optional LoRA adapter file, folded in at load.
    pub adapter: Option<std::path::PathBuf>,
    /// Inline context budget (tokens).
    pub context_tokens: u32,
}

impl LocalQwen {
    /// Loads weights, tokenizer and (optionally) an adapter, and builds the
    /// decode engine. This is the expensive step - do it once, at startup,
    /// not per request.
    pub fn load(weights: &LocalWeights, model_name: &str) -> anyhow::Result<Self> {
        let base = resolve_base(&weights.base)
            .with_context(|| format!("resolving weights at {}", weights.base.display()))?;
        let base = base.to_string_lossy().into_owned();
        let reader = WeightReader::open(&base).map_err(|e| anyhow::anyhow!("{base}: {e}"))?;
        // Tokenizer precedence, matching brain's own resident loader: an
        // explicit sibling tokenizer.json wins; a GGUF carries one embedded.
        let tokenizer_path = weights.base.join("tokenizer.json");
        let tok = if tokenizer_path.is_file() {
            let path = tokenizer_path.to_string_lossy().into_owned();
            QwenBpe::from_file(&path).map_err(|e| anyhow::anyhow!("{path}: {e}"))?
        } else if let Some(gt) = reader.tokenizer() {
            QwenBpe::from_gguf(&gt)
                .map_err(|e| anyhow::anyhow!("loading tokenizer from GGUF metadata: {e}"))?
        } else {
            anyhow::bail!(
                "no tokenizer: expected {} beside the checkpoint",
                tokenizer_path.display()
            )
        };
        let eos = tok
            .encode("<|im_end|>")
            .first()
            .copied()
            .map(|t| vec![t])
            .unwrap_or_default();

        let ctx = weights.context_tokens.max(1);
        // Adapter serving is the `from_tensors_decode` path - the fold the
        // qwen3 crate documents for exactly this. Base-only stays on the
        // mmap streaming load, which never materializes the whole model on
        // the host.
        let model = if let Some(adapter) = &weights.adapter {
            let adapter = adapter.to_string_lossy().into_owned();
            let mut tensors = checkpoint::load(&base).into_by_role("");
            lora::fold_adapter_into(&mut tensors, &adapter)
                .map_err(|e| anyhow::anyhow!("folding adapter {adapter}: {e}"))?;
            let mut cfg = qwen3::config::QwenConfig::from_json(&reader.config());
            cfg.lora = None;
            Qwen::from_tensors_decode(cfg, &tensors, ctx)
        } else {
            Qwen::from_reader_decode(&reader, ctx)
        };
        // The (tied) LM head is applied host-side by the sampler; read it
        // once here, off the built model, so both load paths - including the
        // adapter fold - go through one head derivation.
        let head = model.read_weight(model.cfg.head_weight());
        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            head: Arc::new(head),
            tok: Arc::new(tok),
            eos: Arc::new(eos),
            model_name: model_name.to_string(),
            max_new_tokens: DEFAULT_MAX_NEW_TOKENS,
            temperature: DEFAULT_TEMPERATURE,
            context_tokens: ctx,
            in_flight: Arc::new(Mutex::new(None)),
            live: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Cancels the in-flight generation, if any, and waits up to `grace` for
    /// it to actually stop. The runner calls this on a raced attempt end -
    /// timeout, interrupt - so the process exits AFTER the device is quiet:
    /// exiting under a live prefill or decode crashed the process instead of
    /// ending it. Returns whether every generation has stopped; on `false`
    /// the caller should treat the exit as best-effort (the generation is
    /// cancelling and will stop, just not inside the grace window).
    #[must_use]
    pub fn stop_generation(&self, grace: std::time::Duration) -> bool {
        if let Some(cancel) = self
            .in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            cancel.cancel();
        }
        let deadline = std::time::Instant::now() + grace;
        while self.live.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.live.load(Ordering::SeqCst) == 0
    }
}

/// A directory pointing at a checkpoint resolves to the checkpoint inside
/// it; a file passes through. Mirrors brain's own `resolve_base`.
fn resolve_base(specified: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    if specified.is_file() {
        return Ok(specified.to_path_buf());
    }
    for name in ["model.safetensors", "model.brain.safetensors"] {
        let candidate = specified.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    // Anything else: refuse here, where the caller can name the directory,
    // rather than inside the checkpoint open.
    anyhow::bail!("no checkpoint file found under {}", specified.display())
}

#[async_trait::async_trait]
impl ModelProvider for LocalQwen {
    fn name(&self) -> &str {
        "brain"
    }

    fn model_name(&self) -> &str {
        &self.model_name
    }

    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> anyhow::Result<sven_sdk::model::ResponseStream> {
        let invocation = invocation_from(&req, self.max_new_tokens, self.temperature)?;
        // Generation owns the model's KV cache; hold the lock across the
        // whole decode, off the async runtime's threads. The Arcs make the
        // generation closure `'static` without copying the (hundreds-of-MB)
        // head per request.
        let (model, head, tok, eos) = (
            Arc::clone(&self.model),
            Arc::clone(&self.head),
            Arc::clone(&self.tok),
            Arc::clone(&self.eos),
        );
        let ctx = self.context_tokens;
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let cancel = CancelToken::armed();
        // Register this generation so a raced attempt end can stop it
        // through `stop_generation`, and account it in `live` for that
        // method's bounded wait.
        if let Some(slot) = self
            .in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            slot.cancel();
        }
        *self.in_flight.lock().unwrap_or_else(|e| e.into_inner()) = Some(cancel.clone());
        self.live.fetch_add(1, Ordering::SeqCst);
        let live = Arc::clone(&self.live);
        // Generation runs on an OS thread the async runtime does not own and
        // never joins. A `spawn_blocking` task would make the runtime's
        // shutdown wait for a decode to run to its cap - an abandoned turn
        // (timeout, interrupt) hung the process for minutes inside
        // `Runtime::drop`. Here an abandoned stream is what arms the cancel:
        // its receiver is gone, the next send fails, and the decode stops at
        // the next step boundary. The runner additionally calls
        // `stop_generation` before exiting, so the device is quiet - not
        // merely abandoned - when the process ends.
        std::thread::Builder::new()
            .name("loop-generate".to_string())
            .spawn(move || {
                // A device error surfaces as a panic (brain's backend reports
                // wgpu errors that way); catch it here so it reaches the
                // consumer as a failed stream instead of a dead thread.
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let model = model
                        .lock()
                        .map_err(|e| anyhow::anyhow!("model lock poisoned: {e}"))?;
                    let gen = Generation {
                        head: &head,
                        tok: &tok,
                        eos: &eos,
                        context_tokens: ctx,
                        tx: &tx,
                        cancel: &cancel,
                    };
                    generate_once(&model, &invocation, &gen)
                }));
                let tail = match run {
                    Ok(Ok(outcome)) => events_from(outcome),
                    Ok(Err(e)) => vec![Err(anyhow::anyhow!("generation failed: {e:#}"))],
                    Err(panic) => vec![Err(anyhow::anyhow!(
                        "device panic during generation: {}",
                        panic_message(&panic)
                    ))],
                };
                for event in tail {
                    let _ = tx.blocking_send(event);
                }
                // Drop the live count - `stop_generation`'s wait condition.
                // The slot itself keeps the (now-finished) token: cancelling
                // a finished generation is a no-op, and the next request
                // replaces the slot wholesale.
                live.fetch_sub(1, Ordering::SeqCst);
            })
            .map_err(|e| anyhow::anyhow!("spawning the generation thread: {e}"))?;
        Ok(Box::pin(Events(rx)))
    }
}

/// The generation thread's events as sven's `ResponseStream`: the receiving
/// half of the channel the decode streams through. Dropping it is the
/// abandon signal - the sender sees a failed send and arms the cancel token.
struct Events(tokio::sync::mpsc::Receiver<anyhow::Result<ResponseEvent>>);

impl futures::Stream for Events {
    type Item = anyhow::Result<ResponseEvent>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

/// A panic payload as text, however it was constructed.
#[must_use]
pub(crate) fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_else(|| "no message".into())
}

/// Maps sven's request onto the invocation brain's chat parser reads. The
/// message/tool shapes are the OpenAI wire shapes brain's
/// `parse_chat_messages`/`parse_tools` accept - sven's types are serialized
/// into those shapes rather than re-invented here.
fn invocation_from(
    req: &CompletionRequest,
    max_new: usize,
    temperature: f64,
) -> anyhow::Result<Invocation> {
    let messages: Vec<serde_json::Value> = req.messages.iter().map(message_json).collect();
    let tools: Vec<serde_json::Value> = req.tools.iter().map(tool_json).collect();
    let mut inv = Invocation::new();
    if !messages.is_empty() {
        inv = inv.set(
            "messages",
            serde_json::Value::String(
                serde_json::to_string(&messages).context("serializing messages")?,
            ),
        );
    }
    if !tools.is_empty() {
        inv = inv.set(
            "tools",
            serde_json::Value::String(serde_json::to_string(&tools).context("serializing tools")?),
        );
    }
    inv = inv
        .set("max_new", serde_json::json!(max_new))
        .set("temp", serde_json::json!(temperature))
        .set("top_k", serde_json::json!(DEFAULT_TOP_K))
        // Agent work wants the answer, not a reasoning preamble it cannot
        // use as tool input.
        .set("enable_thinking", serde_json::json!(false));
    Ok(inv)
}

/// One sven message as brain's chat parser reads it. A tool result rides in
/// as `role: "tool"` with its call id; an assistant tool request rides out
/// as `tool_calls`, so the template renders the exchange the model itself
/// produced.
#[must_use]
fn message_json(message: &Message) -> serde_json::Value {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let mut value = serde_json::json!({
        "role": role,
        "content": content_text(message),
    });
    match &message.content {
        MessageContent::ToolCall {
            tool_call_id,
            function,
        } => {
            value["tool_calls"] = serde_json::json!([{
                "id": tool_call_id,
                "function": {"name": function.name, "arguments": function.arguments},
            }]);
            value["content"] = serde_json::Value::String(String::new());
        }
        MessageContent::ToolResult {
            tool_call_id,
            content,
        } => {
            value["tool_call_id"] = serde_json::json!(tool_call_id);
            if let Some(text) = content.as_text() {
                value["content"] = serde_json::json!(text);
            }
        }
        _ => {}
    }
    value
}

/// The message's text content: plain text verbatim, mixed parts joined. A
/// tool-result part array keeps its text rather than disappearing.
#[must_use]
fn content_text(message: &Message) -> String {
    match &message.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::ContentParts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// One sven tool schema as an OpenAI-shaped function object, the shape
/// brain's tool parser accepts.
#[must_use]
fn tool_json(tool: &ToolSchema) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        },
    })
}

/// Everything one generation reads beyond the locked model and the parsed
/// request: the sampler's head, tokenizer and stop tokens, the inline
/// context budget, the stream visible text goes out on, and the token an
/// abandoned turn arms.
struct Generation<'a> {
    head: &'a [f32],
    tok: &'a QwenBpe,
    eos: &'a [u32],
    context_tokens: u32,
    tx: &'a tokio::sync::mpsc::Sender<anyhow::Result<ResponseEvent>>,
    cancel: &'a CancelToken,
}

/// One generation, start to finish: render, decode, scan, finish. Runs on a
/// dedicated OS thread with the model lock held, streaming visible text
/// through the stream as the scanner produces it.
///
/// Cancellation is cooperative: the token is polled between prefill chunks
/// (sized by [`PREFILL_CHUNK_TOKENS`]) and between decode steps, never
/// inside one (brain's documented contract). A dropped receiver - the turn
/// above this stream was abandoned - arms it through the failed send; the
/// runner's `stop_generation` arms it directly. Either way an interrupted
/// turn stops within one chunk of where it is, not after the whole prompt
/// or the whole generation cap.
fn generate_once(model: &Qwen, inv: &Invocation, gen: &Generation<'_>) -> anyhow::Result<Outcome> {
    let req = chat::parse_request(gen.tok, inv).map_err(anyhow::Error::msg)?;
    let context = usize::try_from(gen.context_tokens).unwrap_or(usize::MAX);
    anyhow::ensure!(
        req.ids.len() + req.max_new <= context,
        "prompt ({} tokens) plus generation ({}) exceeds the engine's context ({context})",
        req.ids.len(),
        req.max_new
    );
    let mut rng = Rng::new(req.seed);
    // A clone of the token outlives the sequence: the emit path arms it when
    // the consumer above this stream is gone.
    let abandon = gen.cancel.clone();
    let mut seq = SeqState::new(&req, gen.cancel.clone());
    let mut ids_out: Vec<u32> = Vec::with_capacity(req.max_new);
    // A failed send means the consumer is gone - the turn was abandoned
    // above this stream - so arm the token; the next `advance` observes it
    // and ends the sequence.
    let emit = &mut |p: Progress| {
        if let Some(text) = p.delta {
            if gen
                .tx
                .blocking_send(Ok(ResponseEvent::TextDelta(text)))
                .is_err()
            {
                abandon.cancel();
            }
        }
    };
    let generated = generate_kv_stream_cancellable(
        model,
        &req.ids,
        req.max_new,
        req.temp,
        req.top_k,
        req.top_p,
        gen.eos,
        &mut rng,
        gen.head,
        gen.cancel,
        PREFILL_CHUNK_TOKENS,
        &mut |_i, t| {
            ids_out.push(t);
            // `advance` answers "should we stop?"; the callback answers
            // "keep going?" - the same inversion the serving path applies.
            !seq.advance(gen.tok, &ids_out, emit)
        },
    );
    // `finish` flushes the scanner's held-back tail through the same emit,
    // so the streamed deltas and the outcome's text stay identical.
    Ok(seq.finish(gen.tok, &generated, emit))
}

/// The finished outcome's non-text events. Visible text was streamed while
/// it was scanned; the tail carries the tool calls the scanner extracted -
/// complete, post-finish - then usage, then done.
fn events_from(outcome: Outcome) -> Vec<anyhow::Result<ResponseEvent>> {
    let mut events = Vec::new();
    if let Some(calls) = outcome
        .outputs
        .get("tool_calls")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.as_array().cloned())
    {
        for (index, call) in calls.iter().enumerate() {
            let arguments = match call.get("arguments") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            events.push(Ok(ResponseEvent::ToolCall {
                index: index as u32,
                id: call
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                name: call
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                arguments,
            }));
        }
    }
    events.push(Ok(ResponseEvent::Usage {
        input_tokens: outcome
            .outputs
            .get("prompt_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as u32,
        output_tokens: outcome
            .outputs
            .get("completion_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as u32,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd: None,
    }));
    events.push(Ok(ResponseEvent::Done));
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_sdk::model::{ContentPart, FunctionCall, ToolResultContent};

    fn message(role: Role, content: MessageContent) -> Message {
        Message { role, content }
    }

    #[test]
    fn sven_messages_map_onto_the_openai_shapes_brains_parser_reads() {
        let user = message(Role::User, MessageContent::Text("do the thing".into()));
        let assistant = message(
            Role::Assistant,
            MessageContent::ToolCall {
                tool_call_id: "call_1".into(),
                function: FunctionCall {
                    name: "write".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            },
        );
        let tool = message(
            Role::Tool,
            MessageContent::ToolResult {
                tool_call_id: "call_1".into(),
                content: ToolResultContent::Text("wrote 5 bytes".into()),
            },
        );
        let mapped: Vec<serde_json::Value> = [&user, &assistant, &tool]
            .iter()
            .map(|m| message_json(m))
            .collect();
        assert_eq!(mapped[0]["role"], "user");
        assert_eq!(mapped[1]["tool_calls"][0]["function"]["name"], "write");
        assert_eq!(mapped[2]["role"], "tool");
        assert_eq!(mapped[2]["tool_call_id"], "call_1");
        assert_eq!(mapped[2]["content"], "wrote 5 bytes");
        // Round-trip through brain's own parser - the consumer this feeds.
        let raw = serde_json::to_string(&mapped).unwrap();
        let parsed = chat::parse_chat_messages(&raw, None).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[1].tool_calls[0].name, "write");
        assert_eq!(parsed[1].tool_calls[0].arguments, r#"{"path":"a.txt"}"#);
        assert_eq!(parsed[2].role, data::qwen_chat::Role::Tool);
        assert_eq!(parsed[2].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn sven_tool_schemas_map_onto_openai_function_objects() {
        let tool = ToolSchema {
            name: "read".into(),
            description: "read a file".into(),
            parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
            is_mcp: false,
        };
        let mapped = tool_json(&tool);
        // The tools param is the whole array, as the invocation builds it.
        let raw = serde_json::to_string(&vec![mapped]).unwrap();
        let parsed = chat::parse_tools(Some(&raw)).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].contains("read"));
    }

    #[test]
    fn a_finished_outcomes_tail_is_tool_calls_usage_and_done() {
        let outcome = Outcome::new()
            .set("text", serde_json::json!("did it"))
            .set("prompt_tokens", serde_json::json!(120))
            .set("completion_tokens", serde_json::json!(34))
            .set(
                "tool_calls",
                serde_json::json!(r#"[{"id":"c1","name":"write","arguments":"{}"}]"#),
            );
        let events = events_from(outcome);
        // The visible text was streamed while it was scanned; the tail must
        // not repeat it.
        assert_eq!(events.len(), 3, "tool call, usage, done");
        assert!(matches!(
            &events[0],
            Ok(ResponseEvent::ToolCall { name, arguments, .. })
                if name == "write" && arguments == "{}"
        ));
        assert!(matches!(
            &events[1],
            Ok(ResponseEvent::Usage { input_tokens, output_tokens, cost_usd: None, .. })
                if *input_tokens == 120 && *output_tokens == 34
        ));
        assert!(matches!(events[2], Ok(ResponseEvent::Done)));
    }

    #[test]
    fn mixed_content_parts_keep_their_text() {
        let message = message(
            Role::User,
            MessageContent::ContentParts(vec![ContentPart::Text {
                text: "see ".into(),
            }]),
        );
        assert_eq!(content_text(&message), "see ");
    }

    #[test]
    fn a_directory_of_the_standard_layout_resolves_to_its_checkpoint() {
        let dir = std::env::temp_dir().join(format!("loop-provider-base-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(resolve_base(&dir).is_err(), "nothing inside, no resolution");
        std::fs::write(dir.join("model.safetensors"), b"x").unwrap();
        assert_eq!(resolve_base(&dir).unwrap(), dir.join("model.safetensors"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
