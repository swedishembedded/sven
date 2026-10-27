// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `document_convert` tool.
//!
//! It converts a PDF to a Markdown file and returns a control record: the page
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
use sven_document::{ConvertOptions, Converter, DocumentError, HayroBackend, OcrMode, Report};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};
use sven_tool_api::PathScope;
use sven_vocab::AgentMode;

use crate::models::{NoOcrModel, OcrModels};

/// Warnings returned in the record; the manifest holds all of them.
const MAX_WARNINGS: usize = 8;

pub struct DocumentConvertTool {
    scope: PathScope,
    models: Arc<dyn OcrModels>,
    defaults: ConvertOptions,
}

impl DocumentConvertTool {
    pub fn new(scope: PathScope, models: Arc<dyn OcrModels>, defaults: ConvertOptions) -> Self {
        Self {
            scope,
            models,
            defaults,
        }
    }
}

#[async_trait]
impl Tool for DocumentConvertTool {
    fn name(&self) -> &str {
        "document_convert"
    }

    fn summary(&self) -> &str {
        "Convert a PDF file to a Markdown file (OCR for scanned pages). Returns status, not text."
    }

    fn description(&self) -> &str {
        "Convert a PDF (or the pages of it you select) to a Markdown file with an OCR model. \
         Returns a status record, not the text; read the destination only if you need the \
         content. To convert one chapter or section, select its page range from the PDF's outline."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "source": {"type": "string", "description": "PDF file"},
                "destination": {"type": "string", "description": "Markdown file to write"},
                "model": {"type": "string", "description": "OCR model, loose name ok: 'deepseek ocr v2'"},
                "pages": {"type": "string", "description": "e.g. 1-5,9"},
                "ocr": {"type": "string", "enum": ["auto", "always", "never"]}
            },
            "required": ["source", "destination"],
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

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let arg = |name: &str| call.args.get(name).and_then(Value::as_str);
        let (Some(source), Some(destination)) = (arg("source"), arg("destination")) else {
            return ToolOutput::err(
                &call.id,
                "Missing required parameters: source, destination.",
            );
        };
        let ocr = match arg("ocr").map(str::parse::<OcrMode>).transpose() {
            Ok(mode) => mode.unwrap_or_default(),
            Err(e) => return ToolOutput::err(&call.id, e),
        };
        let source = match self.scope.resolve_for(call, source) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let destination = match self.scope.resolve_for(call, destination) {
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
        let converter = Converter::new(Arc::new(HayroBackend), providers);
        match converter.convert(&source, &destination, &options).await {
            Ok(report) => ToolOutput::ok(&call.id, record(&report, &spec, unavailable.as_deref())),
            Err(DocumentError::NoModel) => ToolOutput::ok(
                &call.id,
                no_ocr_model_record(&unavailable.unwrap_or_default()),
            ),
            Err(e) => ToolOutput::err(&call.id, failure_text(&e.to_string())),
        }
    }
}

/// The control record: counts, hashes and paths, never page text.
/// `no_model` is why no OCR model was available, when none was.
fn record(report: &Report, model: &str, no_model: Option<&str>) -> String {
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
            "blank": report.pages_blank,
            "reused": report.pages_reused,
            "warning": report.pages_warning,
            "failed": report.pages_failed,
        },
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
    record.to_string()
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
fn no_ocr_model_record(reason: &str) -> String {
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

    struct Fixed(Result<OcrModel, NoOcrModel>);

    #[async_trait]
    impl OcrModels for Fixed {
        async fn resolve(&self, _requested: Option<&str>) -> Result<OcrModel, NoOcrModel> {
            self.0.clone()
        }
    }

    fn tool(dir: &std::path::Path, models: Result<OcrModel, NoOcrModel>) -> DocumentConvertTool {
        DocumentConvertTool::new(
            PathScope::confined(dir).unwrap(),
            Arc::new(Fixed(models)),
            ConvertOptions::default(),
        )
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
    }

    #[test]
    fn the_definition_stays_small() {
        let t = tool(std::path::Path::new("/"), model());
        assert!(t.summary().chars().count() <= sven_tool_api::SUMMARY_MAX_CHARS);
        assert!(t.parameters_schema().to_string().len() < 900);
    }
}
