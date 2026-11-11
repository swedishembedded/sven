// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `document_convert` tool.
//!
//! It converts a document (a PDF or a picture of a page) to a Markdown file
//! and returns a control record: the page
//! text goes from the OCR model to the file and never through the chat
//! model's context.
//!
//! Swedish Embedded AB implements agent tools that move bulk data around the
//! language model instead of through it. If your team needs expertise in
//! building cost-efficient document agents then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_document::{manifest_path, ConvertOptions, Converter, DocumentError, OcrMode, Report};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};
use sven_tool_api::ToolEvent;
use sven_vocab::AgentMode;

use crate::access::DocumentAccess;
use crate::models::{NoOcrModel, OcrModels};
use crate::scoped;

/// The longest one conversion may run before the executor's watchdog ends
/// it (the next call resumes from the manifest).
const LONGEST_CONVERSION: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// Warnings returned in the record; the manifest holds all of them.
const MAX_WARNINGS: usize = 8;

pub struct DocumentConvertTool {
    access: DocumentAccess,
    models: Arc<dyn OcrModels>,
    defaults: ConvertOptions,
    /// Whether the session can run commands, so offering installed
    /// command-line converters when no OCR model exists makes sense.
    commands: bool,
    /// Where it says how far a conversion is, and at most how often.
    progress: Option<(tokio::sync::mpsc::Sender<ToolEvent>, std::time::Duration)>,
}

impl DocumentConvertTool {
    pub fn new(
        access: DocumentAccess,
        models: Arc<dyn OcrModels>,
        defaults: ConvertOptions,
    ) -> Self {
        Self {
            access,
            models,
            defaults,
            commands: true,
            progress: None,
        }
    }

    /// Reports how far each conversion is on `events`, at most every
    /// `every`: a long conversion is then seen working (and its answer kept
    /// open) rather than silent for an hour.
    #[must_use]
    pub fn with_progress(
        mut self,
        events: tokio::sync::mpsc::Sender<ToolEvent>,
        every: std::time::Duration,
    ) -> Self {
        self.progress = Some((events, every));
        self
    }

    /// For a session that cannot run commands: no command-line converter
    /// is offered when there is no OCR model.
    #[must_use]
    pub fn without_commands(mut self) -> Self {
        self.commands = false;
        self
    }
}

#[async_trait]
impl Tool for DocumentConvertTool {
    fn name(&self) -> &str {
        "document_convert"
    }

    fn summary(&self) -> &str {
        "Convert a PDF or page picture to a Markdown file (OCR for scans). Returns status, not text."
    }

    fn description(&self) -> &str {
        "Convert a PDF (or chosen pages), or a photo or scan of one page, to a Markdown file \
         with an OCR model. Returns a status record, not the text; read the destination only if \
         you need it. For one chapter, choose its pages from the PDF's outline."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "source": {"type": "string", "description": "PDF or picture: a path, or a file handle (mcp-file://...)"},
                "destination": {"type": "string", "description": "Markdown file to write; default <source name>.md"},
                "model": {"type": "string", "description": "OCR model, loose name ok: 'deepseek ocr v2'"},
                "pages": {"type": "string", "description": "e.g. 1-5,9"},
                "ocr": {"type": "string", "enum": ["auto", "always", "never"]}
            },
            "required": ["source"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::WriteFile
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    /// A document of hundreds of pages needs OCR on many of them; every
    /// request has its own deadline, and an interrupted run resumes from its
    /// manifest, so the watchdog only needs to catch one that never ends.
    fn longest_run(&self) -> Option<std::time::Duration> {
        Some(LONGEST_CONVERSION)
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let arg = |name: &str| call.args.get(name).and_then(Value::as_str);
        let Some(source_arg) = arg("source") else {
            return ToolOutput::err(&call.id, "Missing required parameter: source.");
        };
        let ocr = match arg("ocr").map(str::parse::<OcrMode>).transpose() {
            Ok(mode) => mode.unwrap_or_default(),
            Err(e) => return ToolOutput::err(&call.id, e),
        };
        let source_arg = match self.access.source(call, source_arg).await {
            Ok(path) => path,
            Err(refused) => return refused,
        };
        let source_arg = source_arg.as_str();
        // Without a destination, the Markdown is named after the source, in
        // the working directory.
        let destination_arg = match arg("destination") {
            Some(destination) => destination.to_owned(),
            None => format!(
                "{}.md",
                std::path::Path::new(source_arg)
                    .file_stem()
                    .map_or_else(|| "document".into(), |s| s.to_string_lossy())
            ),
        };
        let destination_arg = destination_arg.as_str();
        let destination = match self.access.scope().resolve_for(call, destination_arg) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        // Without a model the pages that have good native text can still be
        // converted; the converter says if any page needs one.
        let (spec, providers, unavailable) = match self.models.resolve(arg("model")).await {
            Ok(m) => (m.spec, m.providers, None),
            Err(NoOcrModel::Refused(message)) => return ToolOutput::err(&call.id, message),
            Err(NoOcrModel::Unavailable { reason }) => {
                ("native".to_string(), Vec::new(), Some(reason))
            }
        };
        let options = ConvertOptions {
            ocr,
            pages: arg("pages").unwrap_or_default().to_string(),
            model_spec: spec.clone(),
            ..self.defaults.clone()
        };
        // The converter works on private copies: the source and any earlier
        // run's manifest are read through the scope, the results written
        // back through it (`scoped`).
        let data = match scoped::read(self.access.scope(), source_arg, false).await {
            Ok(Some(data)) => data,
            Ok(None) => {
                return ToolOutput::err(
                    &call.id,
                    failure_text(&format!("cannot read {source_arg}")),
                )
            }
            Err(why) => return ToolOutput::err(&call.id, failure_text(&why)),
        };
        let manifest_arg = format!("{destination_arg}.manifest.json");
        let earlier = match scoped::read(self.access.scope(), &manifest_arg, true).await {
            Ok(earlier) => earlier,
            Err(why) => return ToolOutput::err(&call.id, failure_text(&why)),
        };
        let work = match tempfile::tempdir() {
            Ok(work) => work,
            Err(e) => {
                return ToolOutput::err(
                    &call.id,
                    failure_text(&format!("no private working directory: {e}")),
                )
            }
        };
        // The copy keeps the source's extension, which says a picture from a PDF.
        let extension = std::path::Path::new(source_arg)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("pdf");
        let (private_source, private_output) = (
            work.path().join(format!("source.{extension}")),
            work.path().join("output.md"),
        );
        let staged = std::fs::write(&private_source, &data).and_then(|()| match &earlier {
            Some(manifest) => std::fs::write(manifest_path(&private_output), manifest),
            None => Ok(()),
        });
        if let Err(e) = staged {
            return ToolOutput::err(
                &call.id,
                failure_text(&format!("cannot stage {source_arg}: {e}")),
            );
        }
        drop(data);
        let mut converter = Converter::new(self.access.backend(), providers);
        if let Some((events, every)) = &self.progress {
            converter =
                converter.with_progress(progress_reporter(events.clone(), *every, &call.id));
        }
        let converted = converter
            .convert(&private_source, &private_output, &options)
            .await;
        // What the run made goes back, even when it stopped part way: the
        // manifest lets a later run resume.
        for (private, requested) in [
            (manifest_path(&private_output), manifest_arg.as_str()),
            (private_output.clone(), destination_arg),
        ] {
            if let Ok(bytes) = std::fs::read(&private) {
                if let Err(why) = scoped::write(self.access.scope(), requested, bytes).await {
                    return ToolOutput::err(&call.id, failure_text(&why));
                }
            }
        }
        let report = match converted {
            Ok(mut report) => {
                report.output = destination.clone();
                report.manifest = manifest_path(&destination);
                report
            }
            Err(DocumentError::NoModel) => {
                return ToolOutput::ok(
                    &call.id,
                    no_ocr_model_record(&unavailable.unwrap_or_default(), self.commands),
                )
            }
            Err(e) => {
                return ToolOutput::err(
                    &call.id,
                    failure_text(
                        &self.access.shown_in(
                            &e.to_string()
                                .replace(&work.path().display().to_string(), "<private copy>"),
                        ),
                    ),
                )
            }
        };
        let mut record = record(&report, &spec, unavailable.as_deref());
        // Paths as the agent names them: relative to where the session
        // works, never where that is on the machine.
        for (key, path) in [("output", &report.output), ("manifest", &report.manifest)] {
            record[key] = json!(self.access.shown(path));
        }
        // In a conversation of a service, the Markdown is its file: the
        // person sees it, and a model reads it by handle.
        let published = match self.access.publish(&report.output, "text/markdown").await {
            Some(Ok(file)) => {
                record["file"] = json!(file.uri);
                vec![file]
            }
            Some(Err(e)) => {
                record["not_published"] = json!(format!(
                    "the Markdown was written but not added to the conversation: {e}"
                ));
                Vec::new()
            }
            None => Vec::new(),
        };
        ToolOutput::ok(&call.id, self.access.shown_in(&record.to_string()))
            .with_artifacts(published)
    }
}

/// A converter's per-page callback that says "page N of M" on `events` for
/// call `call_id`, at most every `every`, and always for the last page.
fn progress_reporter(
    events: tokio::sync::mpsc::Sender<ToolEvent>,
    every: std::time::Duration,
    call_id: &str,
) -> sven_document::Progress {
    let call_id = call_id.to_owned();
    let last = std::sync::Mutex::new(None::<std::time::Instant>);
    Arc::new(move |done, total, _page| {
        let mut last = last.lock().unwrap();
        let due = last.is_none_or(|at| at.elapsed() >= every) || done == total;
        if due {
            *last = Some(std::time::Instant::now());
            // A full channel drops a report: the next one says more.
            let _ = events.try_send(ToolEvent::Progress {
                call_id: call_id.clone(),
                message: format!("page {done} of {total}"),
            });
        }
    })
}

/// The control record: counts, hashes and paths, never page text.
/// `no_model` is why no OCR model was available, when none was.
fn record(report: &Report, model: &str, no_model: Option<&str>) -> Value {
    let status = if report.pages_failed == report.pages_total {
        "failed"
    } else if report.pages_failed > 0 {
        "partial"
    } else if report.pages_warning > 0 {
        "ok_with_warnings"
    } else {
        "ok"
    };
    let shown = report
        .warnings
        .iter()
        .take(MAX_WARNINGS)
        .collect::<Vec<_>>();
    let mut record = json!({
        "status": status,
        "output": report.output,
        "sha256": report.sha256,
        "model": model,
        "pages": {
            "total": report.pages_total,
            "native": report.pages_native,
            "ocr": report.pages_ocr,
            "read_with_others": report.pages_batched,
            "blank": report.pages_blank,
            "reused": report.pages_reused,
            "warning": report.pages_warning,
            "failed": report.pages_failed,
        },
        "notes": report.notes,
        "warnings": shown,
        "warnings_not_shown": report.warnings.len().saturating_sub(MAX_WARNINGS),
        "manifest": report.manifest,
        "seconds": report.elapsed_ms / 1000,
    });
    if report.pages_failed > 0 {
        record["failures"] = json!(report.failures);
        record["instruction_for_assistant"] = json!(failed_pages_instruction(report, status));
        if let Some(reason) = no_model {
            record["no_ocr_model"] = json!(format!(
                "{reason}. Pages without a usable text layer need one: a provider that serves an \
                 OCR model (for example a brain server with DeepSeek-OCR-2), set as `ocr.model`."
            ));
        }
    }
    record
}

/// What the chat model is told after pages failed: say so, give the cause,
/// stop. A model left to itself tries to repair the environment.
fn failed_pages_instruction(report: &Report, status: &str) -> String {
    let outcome = if status == "failed" {
        "No page was converted".to_string()
    } else {
        format!(
            "{} of {} pages were not converted; {} holds the rest, with those pages marked",
            report.pages_failed,
            report.pages_total,
            report.output.display()
        )
    };
    format!(
        "The conversion did not complete. {outcome}. Tell the user exactly this and then stop: \
         the cause(s) are in `failures` ({}). Do not retry, do not install software, do not \
         write scripts and do not run shell commands to work around it. Ask the user how they \
         want to proceed.",
        report.failures.join("; ")
    )
}

/// An error that ends a conversion, worded so that the chat model reports it
/// and stops instead of improvising.
fn failure_text(cause: &str) -> String {
    format!(
        "The conversion failed: {cause}. Tell the user this cause and then stop. Do not retry, \
         do not install software, do not write scripts and do not run shell commands to work \
         around it. Ask the user how they want to proceed."
    )
}

/// Command-line converters worth offering when no OCR model exists.
const FALLBACK_TOOLS: [&str; 5] = ["pdftotext", "mutool", "pandoc", "ocrmypdf", "tesseract"];

/// The result when no OCR model is available. It carries instructions for the
/// chat model: say so, and ask before trying anything else. Nothing is run.
fn no_ocr_model_record(reason: &str, commands: bool) -> String {
    if !commands {
        return json!({
            "status": "no_ocr_model",
            "converted": false,
            "reason": reason,
            "instruction_for_assistant": format!(
                "The document was NOT converted: no OCR model was chosen or found ({reason}). \
                 Pick a model that reads images (`models` with input image and output text; \
                 prefer one made for documents or OCR), say which, and call document_convert \
                 again with it as `model`. If none is listed, tell the user so."
            ),
        })
        .to_string();
    }
    let found: Vec<&str> = FALLBACK_TOOLS
        .into_iter()
        .filter(|t| sven_workspace::skills::which_available(t))
        .collect();
    let installed = if found.is_empty() {
        "none of pdftotext, mutool, pandoc, ocrmypdf or tesseract were found on PATH".to_string()
    } else {
        format!("found on PATH: {}", found.join(", "))
    };
    json!({
        "status": "no_ocr_model",
        "converted": false,
        "reason": reason,
        "command_line_tools": found,
        "instruction_for_assistant": format!(
            "The PDF was NOT converted. Do not call document_convert again and do not run any \
             command yet. Reply to the user with two things. First: no provider currently offers \
             an OCR model ({reason}). To enable conversion they need to add a provider that serves \
             one, for example a brain server with DeepSeek-OCR-2 or another OpenAI-compatible \
             service, and set `ocr.model` to its model spec. Second, ask this question and wait \
             for the answer: \"Do you want me to try converting the PDF with command-line tools \
             that may already be installed ({installed})?\" Only run such a tool if the user \
             answers yes."
        ),
    })
    .to_string()
}

impl ToolDisplay for DocumentConvertTool {
    fn display_name(&self) -> &str {
        "Convert"
    }
    fn category(&self) -> &str {
        "file"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        let name = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("?");
        format!("{} -> {}", name("source"), name("destination"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::OcrModel;
    use sven_document::testing::{build_pdf, FixturePage, PROSE};
    use sven_model_mock::MockProvider;
    use sven_tool_api::PathScope;

    struct Fixed(Result<OcrModel, NoOcrModel>);

    #[async_trait]
    impl OcrModels for Fixed {
        async fn resolve(&self, _requested: Option<&str>) -> Result<OcrModel, NoOcrModel> {
            self.0.clone()
        }
    }

    fn tool(dir: &std::path::Path, models: Result<OcrModel, NoOcrModel>) -> DocumentConvertTool {
        DocumentConvertTool::new(
            DocumentAccess::local(PathScope::confined(dir).unwrap()),
            Arc::new(Fixed(models)),
            ConvertOptions::default(),
        )
    }

    /// A service that keeps one PDF under a handle and takes what is
    /// published, as a conversation's files.
    struct Service {
        dir: std::path::PathBuf,
        published: std::sync::Mutex<Vec<(std::path::PathBuf, String)>>,
    }

    #[async_trait]
    impl sven_tool_api::files::HandleFetcher for Service {
        async fn fetch(&self, handle: &str) -> Result<String, String> {
            if handle != "mcp-file://files/7b1d" {
                return Err("no such file".into());
            }
            std::fs::create_dir_all(self.dir.join(".sven/files")).unwrap();
            std::fs::write(
                self.dir.join(".sven/files/7b1d.pdf"),
                build_pdf(&[FixturePage::Text(PROSE.to_vec())]),
            )
            .unwrap();
            Ok(".sven/files/7b1d.pdf".into())
        }
    }

    #[async_trait]
    impl sven_tool_api::files::FilePublisher for Service {
        async fn publish(
            &self,
            path: &std::path::Path,
            mime_type: &str,
        ) -> Result<sven_tool_api::files::ArtifactRef, String> {
            self.published
                .lock()
                .unwrap()
                .push((path.to_path_buf(), mime_type.to_owned()));
            Ok(sven_tool_api::files::ArtifactRef {
                uri: "mcp-file://files/md-1".into(),
                name: path.file_name().unwrap().to_string_lossy().into_owned(),
                mime_type: mime_type.to_owned(),
                size: std::fs::metadata(path).unwrap().len(),
            })
        }
    }

    #[tokio::test]
    async fn a_file_of_the_conversation_is_converted_by_handle_and_its_markdown_published() {
        let dir = tempfile::tempdir().unwrap();
        let service = Arc::new(Service {
            dir: dir.path().to_path_buf(),
            published: std::sync::Mutex::default(),
        });
        let access = DocumentAccess::local(PathScope::confined(dir.path()).unwrap())
            .with_service(service.clone(), service.clone());
        let tool =
            DocumentConvertTool::new(access, Arc::new(Fixed(model())), ConvertOptions::default());
        let out = tool
            .execute(&call(
                json!({"source": "mcp-file://files/7b1d", "ocr": "never"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let record: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(record["file"], "mcp-file://files/md-1");
        assert_eq!(out.artifacts.len(), 1);
        assert_eq!(out.artifacts[0].mime_type, "text/markdown");
        let published = service.published.lock().unwrap().clone();
        assert_eq!(published.len(), 1);
        // Named after the source, beside nothing the agent chose.
        assert!(published[0].0.ends_with("7b1d.md"), "{:?}", published[0].0);
        assert!(!out.content.contains("committee"));
        // A handle the service does not keep is refused before anything runs.
        let missing = tool
            .execute(&call(json!({"source": "mcp-file://files/nope"})))
            .await;
        assert!(
            missing.is_error && missing.content.contains("no such file"),
            "{}",
            missing.content
        );
    }

    #[tokio::test]
    async fn a_conversion_reports_how_far_it_is_while_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.pdf"),
            build_pdf(&[
                FixturePage::Text(PROSE.to_vec()),
                FixturePage::Text(PROSE.to_vec()),
            ]),
        )
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let out = tool(dir.path(), model())
            .with_progress(tx, std::time::Duration::ZERO)
            .execute(&call(json!({"source": "a.pdf", "ocr": "never"})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let mut said = Vec::new();
        while let Ok(sven_tool_api::ToolEvent::Progress { call_id, message }) = rx.try_recv() {
            assert_eq!(call_id, "c1");
            said.push(message);
        }
        assert_eq!(
            said.last().map(String::as_str),
            Some("page 2 of 2"),
            "{said:?}"
        );
    }

    #[tokio::test]
    async fn without_a_destination_the_markdown_is_named_after_its_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("in")).unwrap();
        std::fs::write(
            dir.path().join("in/report.pdf"),
            build_pdf(&[FixturePage::Text(PROSE.to_vec())]),
        )
        .unwrap();
        let out = tool(dir.path(), model())
            .execute(&call(json!({"source": "in/report.pdf", "ocr": "never"})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(dir.path().join("report.md").exists());
        // No service, nothing published.
        assert!(out.artifacts.is_empty());
        // Where the session works is not said: a hosted host's directory
        // tells how the server lays out every account's.
        let record: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(record["output"], "report.md");
        assert_eq!(record["manifest"], "report.md.manifest.json");
        let root = dir.path().canonicalize().unwrap();
        assert!(
            !out.content.contains(&*root.to_string_lossy()),
            "{}",
            out.content
        );
    }

    fn call(args: Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: "document_convert".into(),
            args,
        }
    }

    fn model() -> Result<OcrModel, NoOcrModel> {
        Ok(OcrModel {
            spec: "mock/ocr".into(),
            providers: vec![Arc::new(MockProvider)],
        })
    }

    #[tokio::test]
    async fn the_result_is_a_control_record_and_never_the_document_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.pdf"),
            build_pdf(&[FixturePage::Text(PROSE.to_vec())]),
        )
        .unwrap();
        let out = tool(dir.path(), model())
            .execute(&call(
                json!({"source": "a.pdf", "destination": "out/a.md", "ocr": "never"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let text = out.content;
        assert!(
            !text.contains("committee"),
            "page text leaked into the record: {text}"
        );
        let record: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(record["status"], "ok");
        assert_eq!(record["pages"]["native"], 1);
        assert!(std::fs::read_to_string(dir.path().join("out/a.md"))
            .unwrap()
            .contains("committee"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_confined_run_converts_through_the_race_free_scope_and_resumes_from_its_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.pdf"),
            build_pdf(&[FixturePage::Text(PROSE.to_vec())]),
        )
        .unwrap();
        let t = DocumentConvertTool::new(
            DocumentAccess::local(
                PathScope::confined(dir.path())
                    .unwrap()
                    .race_free()
                    .unwrap(),
            ),
            Arc::new(Fixed(model())),
            ConvertOptions::default(),
        );
        let args = json!({"source": "a.pdf", "destination": "out/a.md", "ocr": "never"});
        let out = t.execute(&call(args.clone())).await;
        assert!(!out.is_error, "{}", out.content);
        let record: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(record["output"], json!("out/a.md"));
        assert!(std::fs::read_to_string(dir.path().join("out/a.md"))
            .unwrap()
            .contains("committee"));
        assert!(dir.path().join("out/a.md.manifest.json").is_file());
        // Run again: every page is taken from the manifest it left.
        let again: Value = serde_json::from_str(&t.execute(&call(args)).await.content).unwrap();
        assert_eq!(again["pages"]["reused"], 1, "{again}");
        // A source reached through a link out of the workspace is refused.
        std::os::unix::fs::symlink("/etc", dir.path().join("etc")).unwrap();
        let out = t
            .execute(&call(
                json!({"source": "etc/hostname", "destination": "b.md"}),
            ))
            .await;
        assert!(out.is_error, "{}", out.content);
    }

    #[tokio::test]
    async fn paths_outside_the_workspace_and_unknown_models_are_refused_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(dir.path(), model());
        let out = t
            .execute(&call(
                json!({"source": "a.pdf", "destination": "/etc/passwd.md"}),
            ))
            .await;
        assert!(
            out.is_error && out.content.contains("outside"),
            "{}",
            out.content
        );

        let t = tool(
            dir.path(),
            Err(NoOcrModel::Refused(
                "no OCR model matches 'x'. Available: brain/a".into(),
            )),
        );
        let out = t
            .execute(&call(
                json!({"source": "a.pdf", "destination": "b.md", "model": "x"}),
            ))
            .await;
        assert!(out.is_error && out.content.contains("Available: brain/a"));
    }

    #[tokio::test]
    async fn without_an_ocr_model_text_pages_convert_and_scans_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.pdf"),
            build_pdf(&[FixturePage::Text(PROSE.to_vec()), FixturePage::Scan(100)]),
        )
        .unwrap();
        let t = tool(
            dir.path(),
            Err(NoOcrModel::Unavailable {
                reason: "no provider lists one".into(),
            }),
        );
        let out = t
            .execute(&call(json!({"source": "a.pdf", "destination": "b.md"})))
            .await;
        let record: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(record["status"], "partial", "{record}");
        assert_eq!(record["pages"]["native"], 1);
        assert!(record["failures"][0]
            .as_str()
            .unwrap()
            .starts_with("pages 2: "));
        assert!(record["no_ocr_model"]
            .as_str()
            .unwrap()
            .contains("no provider lists one"));
        assert!(dir.path().join("b.md").exists());
    }

    #[tokio::test]
    async fn without_an_ocr_model_the_result_tells_the_assistant_to_report_and_ask() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.pdf"),
            build_pdf(&[FixturePage::Scan(100)]),
        )
        .unwrap();
        let t = tool(
            dir.path(),
            Err(NoOcrModel::Unavailable {
                reason: "no provider lists one".into(),
            }),
        );
        let out = t
            .execute(&call(json!({"source": "a.pdf", "destination": "b.md"})))
            .await;
        assert!(!out.is_error, "a defined outcome, not a malfunction");
        let record: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(record["status"], "no_ocr_model");
        assert_eq!(record["converted"], false);
        let say = record["instruction_for_assistant"].as_str().unwrap();
        for needed in [
            "ocr.model",
            "DeepSeek-OCR-2",
            "Do you want me to try",
            "command-line tools",
        ] {
            assert!(say.contains(needed), "{needed} missing from: {say}");
        }
        assert!(!dir.path().join("b.md").exists());
    }

    /// A model that accepts images and refuses every request, as a server does
    /// when the page does not fit its budget.
    struct Refusing;

    #[async_trait]
    impl sven_model::ModelProvider for Refusing {
        fn name(&self) -> &str {
            "refusing"
        }
        fn model_name(&self) -> &str {
            "refusing-ocr"
        }
        fn input_modalities(&self) -> Vec<sven_model::catalog::InputModality> {
            vec![sven_model::catalog::InputModality::Image]
        }
        async fn complete(
            &self,
            _req: sven_model::CompletionRequest,
        ) -> anyhow::Result<sven_model::ResponseStream> {
            anyhow::bail!("brain error 400: the page is too large for that device's budget")
        }
    }

    #[tokio::test]
    async fn a_failed_conversion_tells_the_assistant_the_cause_and_to_stop() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.pdf"),
            build_pdf(&[FixturePage::Scan(100)]),
        )
        .unwrap();
        let models = Ok(OcrModel {
            spec: "refusing/ocr".into(),
            providers: vec![Arc::new(Refusing)],
        });
        let out = tool(dir.path(), models)
            .execute(&call(json!({"source": "a.pdf", "destination": "b.md"})))
            .await;
        assert!(!out.is_error, "a defined outcome: {}", out.content);
        let record: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(record["status"], "failed");
        assert_eq!(record["pages"]["failed"], 1);
        let say = record["instruction_for_assistant"].as_str().unwrap();
        assert!(say.contains("too large for that device's budget"), "{say}");
        for forbidden in ["Do not retry", "do not install", "shell commands"] {
            assert!(say.contains(forbidden), "{forbidden} missing from: {say}");
        }
    }

    #[tokio::test]
    async fn an_unreadable_source_is_reported_with_the_same_instruction() {
        let dir = tempfile::tempdir().unwrap();
        let out = tool(dir.path(), model())
            .execute(&call(
                json!({"source": "missing.pdf", "destination": "b.md"}),
            ))
            .await;
        assert!(out.is_error);
        assert!(
            out.content.contains("cannot read") && out.content.contains("then stop"),
            "{}",
            out.content
        );
        // Named as the agent named it, not where the session works.
        let root = dir.path().canonicalize().unwrap();
        assert!(
            !out.content.contains(&*root.to_string_lossy()),
            "{}",
            out.content
        );
    }

    #[test]
    fn the_definition_stays_small() {
        let t = tool(std::path::Path::new("/"), model());
        assert!(t.summary().chars().count() <= sven_tool_api::SUMMARY_MAX_CHARS);
        assert!(t.parameters_schema().to_string().len() < 900);
    }
}
