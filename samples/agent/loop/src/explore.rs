// SPDX-License-Identifier: Apache-2.0
// (provider selection shared by explore and ask)
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents that grow
// their own training data. If your team needs expertise in data extraction
// from technical documents, you can procure our services by sending an
// email to info@swedishembedded.com.

//! Fact exploration: a markdown fact sheet becomes chat-training records.
//!
//! `explore` splits a document at its headings, asks the configured model
//! to enumerate EVERY factual claim of each section as a question/answer
//! pair, and appends one training record per fact to a JSONL file in the
//! SAME schema `learn` writes to the experience pool - so a trained-on
//! questions file is trainer input without translation.
//!
//! Two rules keep the dataset honest:
//!
//! - Strict parse: a reply that is not exactly one
//!   `{"facts": [{"question", "answer"}, ...]}` object is counted as a
//!   failure for its section and skipped, not salvaged - a half-parsed
//!   reply would silently weight whatever the model felt like emitting.
//! - Dedup by normalized question text: the same fact appearing in two
//!   sections (or re-run over a grown document) trains once.

use crate::provider::LocalWeights;
use crate::store::{self, write_atomic};
use crate::trace::Trace;
use anyhow::Context;
use sven_sdk::model::{CompletionRequest, Message, ModelProvider, ResponseEvent, Role};

/// Everything one exploration needs, as configured by the caller.
#[derive(Clone, Debug)]
pub struct ExploreOptions {
    /// The markdown fact sheet to read.
    pub file: std::path::PathBuf,
    /// The JSONL training file to write atomically at the end.
    pub out: std::path::PathBuf,
    /// Section size cap in lines; a section past it starts a new chunk at
    /// the next heading. `None` means uncapped.
    pub chunk_lines: Option<usize>,
    /// Model selection, exactly as `run` reads it: local weights by
    /// default, `--model` for remote, `--adapter` folds the promotion
    /// pointer.
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub local: Option<LocalWeights>,
}

/// What one exploration produced, for the CLI's summary line.
#[derive(Debug, PartialEq, Eq)]
pub struct ExploreSummary {
    pub run_id: String,
    pub sections: usize,
    pub facts: usize,
    pub parse_failures: usize,
}

/// One strict parse of a facts reply.
#[derive(Debug, PartialEq, Eq)]
pub struct Facts {
    pub pairs: Vec<(String, String)>,
}

/// Splits `text` into sections at markdown headings (`##` / `###`).
///
/// Two invariants:
/// - A table row is never split from its section: only a heading line
///   opens a new section, so a table under a heading stays whole.
/// - `chunk_lines` caps a section's size: when a section exceeds the cap,
///   the NEXT heading starts a fresh chunk (content in flight is kept
///   with the section that holds it - splitting mid-table is what the
///   cap must never cause).
pub(crate) fn split_sections(text: &str, chunk_lines: Option<usize>) -> Vec<String> {
    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut overflow = false;
    for line in text.lines() {
        let is_heading = line.starts_with("## ") || line.starts_with("### ");
        if is_heading && (!current.trim().is_empty() || overflow) {
            sections.push(std::mem::take(&mut current));
            overflow = false;
        }
        current.push_str(line);
        current.push('\n');
        if let Some(cap) = chunk_lines {
            let lines = current.lines().count();
            if lines >= cap {
                // Over the cap: the next heading MUST open a new chunk.
                overflow = true;
            }
        }
    }
    if !current.trim().is_empty() {
        sections.push(current);
    }
    sections
}

/// Strips optional markdown code fences around a reply, so a model that
/// answered perfectly inside ```json fences still parses. Fences are the
/// one tolerated decoration; prose around the object is not.
fn strip_fences(reply: &str) -> &str {
    let trimmed = reply.trim();
    let without = trimmed
        .strip_prefix("```")
        .and_then(|r| r.trim_start_matches(|c: char| c.is_ascii_alphanumeric()).strip_prefix('\n'))
        .unwrap_or(trimmed);
    without
        .strip_suffix("```")
        .map(|r| r.trim())
        .unwrap_or(without)
}

/// Strict parse: the reply must be EXACTLY one JSON object of the shape
/// `{"facts": [{"question": string, "answer": string}, ...]}` after
/// trimming whitespace and stripping code fences. Anything else - prose,
/// an array, a missing key, a non-string field - is a failure.
pub(crate) fn parse_facts_reply(reply: &str) -> anyhow::Result<Facts> {
    let value: serde_json::Value = serde_json::from_str(strip_fences(reply))
        .context("reply is not exactly one JSON object")?;
    let object = value
        .as_object()
        .with_context(|| "reply is not a JSON object".to_string())?;
    let facts = object
        .get("facts")
        .with_context(|| "reply object has no \"facts\" key".to_string())?;
    let list = facts
        .as_array()
        .with_context(|| "\"facts\" is not an array".to_string())?;
    let mut pairs = Vec::new();
    for item in list {
        let entry = item
            .as_object()
            .with_context(|| "a fact entry is not an object".to_string())?;
        let question = entry
            .get("question")
            .and_then(|v| v.as_str())
            .with_context(|| "a fact entry has no string \"question\"".to_string())?;
        let answer = entry
            .get("answer")
            .and_then(|v| v.as_str())
            .with_context(|| "a fact entry has no string \"answer\"".to_string())?;
        pairs.push((question.to_string(), answer.to_string()));
    }
    Ok(Facts { pairs })
}

/// Strict parse for `ask`: the reply must be exactly one
/// `{"answer": string}` object (fences tolerated, prose is not).
pub(crate) fn parse_answer_reply(reply: &str) -> anyhow::Result<String> {
    let value: serde_json::Value = serde_json::from_str(strip_fences(reply)).with_context(|| {
        // A parse failure is a scored event; the raw reply is the evidence
        // a repair decision needs, so it rides in the error chain.
        format!("reply is not exactly one JSON object: {reply:?}")
    })?;
    let object = value
        .as_object()
        .with_context(|| "reply is not a JSON object".to_string())?;
    let answer = object
        .get("answer")
        .and_then(|v| v.as_str())
        .with_context(|| "reply object has no string \"answer\"".to_string())?;
    Ok(answer.to_string())
}

/// Normalized question text for dedup: lowercase, whitespace collapsed.
fn normalize(question: &str) -> String {
    question.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// One training record, in the SAME schema `learn` appends to the pool:
/// the question as context (not supervised), the answer as the supervised
/// turn.
pub(crate) fn training_record(run_id: &str, question: &str, answer: &str) -> serde_json::Value {
    serde_json::json!({
        "messages": [
            { "role": "user", "content": question, "train": false },
            { "role": "assistant", "content": answer, "train": true },
        ],
        "metadata": { "run_id": run_id, "verified_by": [] },
    })
}

/// The exact prompt one section sees. It demands the whole shape, and it
/// says what "covers the section" means: every factual claim, not a sample.
fn facts_prompt(section: &str) -> String {
    format!(
        "Below is one section of a hardware fact sheet. Enumerate EVERY factual claim it \
         makes - every number, unit, limit, relationship and conditional - as question/answer \
         pairs. Do not sample, do not summarize: each distinct fact gets its own pair, and a \
         question must be answerable from this section alone.\n\n\
         Reply with EXACTLY one JSON object and nothing else - no prose, no code fences:\n\
         {{\"facts\": [{{\"question\": string, \"answer\": string}}, ...]}}\n\n\
         SECTION:\n{section}"
    )
}

/// Collects a completion's streamed text into one string. Shared with `ask`.
pub(crate) async fn complete_text(
    provider: &dyn ModelProvider,
    prompt: &str,
) -> anyhow::Result<String> {
    let req = CompletionRequest {
        messages: vec![Message {
            role: Role::User,
            content: sven_sdk::model::MessageContent::Text(prompt.to_string()),
        }],
        // The OpenAI-compat driver parses every response as SSE, so a
        // non-streaming request would return a plain JSON object the
        // parser extracts nothing from - the stream would end empty and
        // every strict parse would fail on it.
        stream: true,
        // A reasoning model spends its output budget on thinking before
        // any answer text arrives; the drivers' 4096-token default ends
        // such a turn at `MaxTokens` with zero visible text. One section
        // asking for EVERY fact needs room for the reasoning AND the
        // object, so the completion carries its own cap. The local
        // provider ignores the override (its 512-token budget and
        // context check are its own), so this stays remote-only.
        max_output_tokens_override: Some(32_768),
        // Observed drift: a full-sheet extraction prompt once drew a
        // markdown answer despite the JSON-only instruction. Drivers that
        // support it accept a JSON-object constraint on the wire; drivers
        // that don't ignore the field, and the strict parse stays the gate.
        response_format: Some(sven_sdk::model::ResponseFormat::JsonObject),
        ..Default::default()
    };
    let mut stream = provider.complete(req).await?;
    let mut text = String::new();
    use futures::StreamExt;
    while let Some(event) = stream.next().await {
        match event? {
            ResponseEvent::TextDelta(delta) => text.push_str(&delta),
            ResponseEvent::Done => break,
            _ => {}
        }
    }
    Ok(text)
}

pub(crate) fn provider_from_shared(options: &ExploreOptions) -> anyhow::Result<Box<dyn ModelProvider>> {
    provider_from(options)
}

/// Builds the model provider exactly as `run` does: local in-process
/// weights when no `--model` was given, else the configured remote driver.
fn provider_from(options: &ExploreOptions) -> anyhow::Result<Box<dyn ModelProvider>> {
    if let Some(spec) = &options.model {
        let (provider, name) = spec
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("--model must be provider/model, got {spec:?}"))?;
        let mut settings = sven_sdk::config::load(None)?;
        settings.model.provider = provider.to_string();
        settings.model.name = name.to_string();
        settings.model.base_url = options.base_url.clone();
        settings.model.api_key = options.api_key.clone();
        return sven_model_drivers::from_config(&settings.model);
    }
    let weights = options
        .local
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no local weights configured"))?;
    let name = crate::runner::local_model_name_of(&weights);
    let provider = crate::provider::LocalQwen::load(&weights, &name)?;
    Ok(Box::new(provider))
}

/// Runs the whole exploration, tracing to its own run dir like `run` does:
/// manifest, one event per section, and an outcome with the counts.
pub(crate) fn run(options: ExploreOptions) -> anyhow::Result<ExploreSummary> {
    let text = std::fs::read_to_string(&options.file)
        .with_context(|| format!("reading {}", options.file.display()))?;
    let sections = split_sections(&text, options.chunk_lines);
    anyhow::ensure!(!sections.is_empty(), "{}: no sections found", options.file.display());

    let run_id = store::new_id_with_prefix("explore");
    let dir = store::run_dir(&run_id);
    let trace = Trace::open(&dir, &run_id, 1)?;

    let mut manifest = store::RunManifest {
        schema: 1,
        run_id: run_id.clone(),
        workspace: options.file.display().to_string(),
        task: format!("explore facts from {}", options.file.display()),
        status: "pending".into(),
        attempts: 1,
        started_ts: crate::clock::utc_now(),
        updated_ts: crate::clock::utc_now(),
        model: match &options.model {
            Some(spec) => spec.clone(),
            None => format!(
                "brain/{}",
                crate::runner::local_model_name_of(
                    options.local.as_ref().expect("local weights")
                )
            ),
        },
        base_url: options.base_url.clone(),
        limits: store::Limits::default(),
    };
    store::write_atomic(
        &dir.join("run.json"),
        &serde_json::to_string_pretty(&manifest)?,
    )?;

    // One provider for the whole run: the load is the expensive step, and
    // every section wants the same model anyway.
    let provider = provider_from(&options)?;
    let rt = tokio::runtime::Runtime::new()?;

    let mut records: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut parse_failures = 0usize;
    for (n, section) in sections.iter().enumerate() {
        let prompt = facts_prompt(section);
        let result: anyhow::Result<String> =
            rt.block_on(async { complete_text(provider.as_ref(), &prompt).await });
        let mut payload = match result {
            Ok(reply) => match parse_facts_reply(&reply) {
                Ok(facts) => {
                    let mut added = 0usize;
                    for (question, answer) in facts.pairs {
                        let key = normalize(&question);
                        if !seen.insert(key) {
                            continue; // already present: skipped, not rewritten
                        }
                        records.push(training_record(&run_id, &question, &answer));
                        added += 1;
                    }
                    serde_json::json!({ "section": n, "facts": added })
                }
                Err(e) => {
                    parse_failures += 1;
                    serde_json::json!({ "section": n, "facts": 0, "parse_failure": format!("{e:#}") })
                }
            },
            Err(e) => {
                parse_failures += 1;
                serde_json::json!({ "section": n, "facts": 0, "completion_error": format!("{e:#}") })
            }
        };
        trace.event("section_explored", &mut payload)?;
    }

    // The dataset is written atomically at the end: a crash mid-exploration
    // leaves no half-file the trainer could read as complete.
    let mut jsonl = String::new();
    for record in &records {
        jsonl.push_str(&serde_json::to_string(record)?);
        jsonl.push('\n');
    }
    write_atomic(&options.out, &jsonl)?;

    let summary = ExploreSummary {
        run_id: run_id.clone(),
        sections: sections.len(),
        facts: records.len(),
        parse_failures,
    };
    let mut outcome = serde_json::json!({
        "schema": 1,
        "run_id": run_id,
        "status": if parse_failures > 0 { "completed_with_parse_failures" } else { "completed" },
        "file": options.file.display().to_string(),
        "out": options.out.display().to_string(),
        "sections": summary.sections,
        "facts": summary.facts,
        "parse_failures": summary.parse_failures,
    });
    trace.event("explore_outcome", &mut outcome)?;
    write_atomic(
        &dir.join("outcome.json"),
        &serde_json::to_string_pretty(&outcome)?,
    )?;
    manifest.status = "completed".into();
    manifest.updated_ts = crate::clock::utc_now();
    store::write_atomic(
        &dir.join("run.json"),
        &serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(summary)
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Headings split; a table stays with its heading's section.
    #[test]
    fn sections_split_at_headings_and_tables_stay_whole() {
        let text = "# Title\n\nintro\n\n## Timers\n\nTIM1 is 16-bit.\n\n\
            | Block | Limit |\n|---|---|\n| SPI1 | 42 Mbit/s |\n\n## Clocks\n\nHSI is 16 MHz.\n";
        let sections = split_sections(text, None);
        assert_eq!(sections.len(), 3, "{sections:#?}");
        assert!(sections[1].contains("TIM1 is 16-bit."));
        // The whole table rides with the section that opened it.
        assert!(sections[1].contains("| SPI1 | 42 Mbit/s |"));
        assert!(sections[2].contains("HSI is 16 MHz."));
    }

    /// A section over the cap starts a new chunk at the next heading - and
    /// content in flight stays with the section that holds it, so the cap
    /// never tears a table apart.
    #[test]
    fn chunk_lines_caps_sections_at_the_next_heading() {
        let text = "## A\n\none\n\n## B\n\ntwo\n\n## C\n\nthree\n";
        let sections = split_sections(text, Some(4));
        assert_eq!(sections.len(), 3, "{sections:#?}");
        assert!(sections[0].starts_with("## A"));
        assert!(sections[1].starts_with("## B"));
        assert!(sections[2].starts_with("## C"));
    }

    /// A well-formed reply parses into its question/answer pairs.
    #[test]
    fn a_well_formed_facts_reply_parses() {
        let reply = r#"{"facts": [
            {"question": "What is the max SPI1 clock?", "answer": "42 Mbit/s"},
            {"question": "How many SPI controllers?", "answer": "3"}
        ]}"#;
        let facts = parse_facts_reply(reply).unwrap();
        assert_eq!(facts.pairs.len(), 2);
        assert_eq!(facts.pairs[0].0, "What is the max SPI1 clock?");
        assert_eq!(facts.pairs[0].1, "42 Mbit/s");
    }

    /// Malformed JSON and prose-wrapped JSON are refusals, not salvages.
    #[test]
    fn malformed_and_prose_wrapped_replies_are_refused() {
        assert!(parse_facts_reply("not json at all").is_err());
        assert!(parse_facts_reply("{\"facts\": [{\"question\": 1}]}").is_err());
        assert!(parse_facts_reply("{\"facts\": []").is_err());
        // An array, not an object.
        assert!(parse_facts_reply("[{\"facts\": []}]").is_err());
        let wrapped = "Here are the facts:\n{\"facts\": [{\"question\": \"q\", \"answer\": \"a\"}]}";
        assert!(parse_facts_reply(wrapped).is_err(), "prose around the object is a parse failure");
    }

    /// Fences ARE tolerated - they are decoration, not prose.
    #[test]
    fn fenced_replies_parse() {
        let reply = "```json\n{\"facts\": [{\"question\": \"q\", \"answer\": \"a\"}]}\n```";
        let facts = parse_facts_reply(reply).unwrap();
        assert_eq!(facts.pairs.len(), 1);
    }

    /// An ask reply parses to its answer string; prose is a refusal.
    #[test]
    fn ask_replies_parse_strictly() {
        assert_eq!(parse_answer_reply("{\"answer\": \"42 Mbit/s\"}").unwrap(), "42 Mbit/s");
        assert_eq!(
            parse_answer_reply("```\n{\"answer\": \"168 MHz\"}\n```").unwrap(),
            "168 MHz"
        );
        assert!(parse_answer_reply("The answer is 168 MHz.").is_err());
        assert!(parse_answer_reply("{\"result\": \"168 MHz\"}").is_err());
    }

    /// Dedup is by normalized question text: case and whitespace collapse.
    #[test]
    fn question_dedup_normalizes_text() {
        assert_eq!(normalize("  What  is the MAX? "), normalize("what is the max?"));
        let mut seen = std::collections::HashSet::new();
        assert!(seen.insert(normalize("What is the max?")));
        assert!(!seen.insert(normalize("what is  the max?")));
    }

    /// An explore-produced record is exactly what learn::read_pool parses.
    #[test]
    fn an_explore_record_is_valid_pool_input() {
        let record = training_record("explore-test", "What is the max?", "42 Mbit/s");
        assert_eq!(
            record["messages"][0],
            serde_json::json!({"role": "user", "content": "What is the max?", "train": false})
        );
        assert_eq!(
            record["messages"][1],
            serde_json::json!({"role": "assistant", "content": "42 Mbit/s", "train": true})
        );
        assert_eq!(record["metadata"]["run_id"], "explore-test");
        assert_eq!(record["metadata"]["verified_by"], serde_json::json!([]));
    }

    /// Captures the request a completion carries, so tests can assert on the
    /// wire contract without a server.
    struct CapturingProvider {
        reply: &'static str,
        seen: std::sync::Mutex<Vec<CompletionRequest>>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for CapturingProvider {
        fn name(&self) -> &str {
            "capturing"
        }
        fn model_name(&self) -> &str {
            "capture-1"
        }
        async fn complete(
            &self,
            req: CompletionRequest,
        ) -> anyhow::Result<sven_sdk::model::ResponseStream> {
            self.seen.lock().unwrap().push(req);
            let reply = self.reply;
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(ResponseEvent::TextDelta(reply.to_string())),
                Ok(ResponseEvent::Done),
            ])))
        }
    }

    /// Facts extraction runs against models that drift out of the requested
    /// shape (a full-sheet prompt produced a markdown answer in one observed
    /// run). Drivers that support it accept a JSON-object constraint, so the
    /// completion must ask for one; the strict parser stays as the gate.
    #[tokio::test]
    async fn completions_constrain_the_reply_to_a_json_object() {
        let provider = CapturingProvider {
            reply: r#"{"facts": []}"#,
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let text = complete_text(&provider, "extract facts").await.unwrap();
        assert_eq!(text, r#"{"facts": []}"#);
        let seen = provider.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].response_format,
            Some(sven_sdk::model::ResponseFormat::JsonObject)
        );
        assert!(seen[0].stream, "driver parses every reply as SSE");
    }
}

