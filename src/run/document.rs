// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;

use sven_bootstrap::{Config, ConfigOcrModels};
use sven_document::{
    character_error_rate, ConvertOptions, Converter, DocumentError, HayroBackend, OcrMode,
};
use sven_tools_document::{NoOcrModel, OcrModels};

use crate::cli::{ConvertArgs, DocumentCommands};

/// Exit status when pages failed (the output is still written).
const EXIT_PARTIAL: i32 = 2;
/// Exit status when no OCR model is available.
const EXIT_NO_MODEL: i32 = 3;

pub(crate) async fn run_document_command(
    cmd: &DocumentCommands,
    config: &Config,
) -> anyhow::Result<i32> {
    match cmd {
        DocumentCommands::Convert(args) => convert(args, config).await,
        DocumentCommands::Score {
            reference,
            hypothesis,
        } => {
            let read = |p: &std::path::Path| {
                std::fs::read_to_string(p)
                    .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", p.display()))
            };
            match character_error_rate(&read(reference)?, &read(hypothesis)?) {
                Some(cer) => println!("{cer:.4}"),
                None => anyhow::bail!("the reference text is empty"),
            }
            Ok(0)
        }
    }
}

async fn convert(args: &ConvertArgs, config: &Config) -> anyhow::Result<i32> {
    let mut options = ConvertOptions {
        ocr: args.ocr.parse::<OcrMode>().map_err(anyhow::Error::msg)?,
        pages: args.pages.clone().unwrap_or_default(),
        max_attempts: args.max_attempts,
        page_markers: args.page_markers,
        dedupe_headers: args.dedupe_headers,
        resume: !args.no_resume,
        ..sven_bootstrap::convert_options(&config.ocr)
    };
    if let Some(v) = args.concurrency {
        options.concurrency = v;
    }
    if let Some(v) = args.dpi {
        options.dpi = v;
    }
    if let Some(v) = args.max_tokens {
        options.max_tokens = v;
    }
    if let Some(v) = &args.prompt {
        options.prompt = v.clone();
    }

    // Pages with good native text need no model, so an unavailable model is
    // only reported when the converter finds a page that needs one.
    let (spec, providers, unavailable) = match ConfigOcrModels::new(config.clone())
        .resolve(args.model.as_deref())
        .await
    {
        Ok(m) => (m.spec, m.providers, None),
        Err(NoOcrModel::Refused(message)) => anyhow::bail!(message),
        Err(NoOcrModel::Unavailable { reason }) => ("native".to_string(), Vec::new(), Some(reason)),
    };
    options.model_spec = spec;

    let mut converter = Converter::new(Arc::new(HayroBackend), providers);
    if !args.quiet {
        converter = converter.with_progress(Arc::new(|done, total, page| {
            eprintln!(
                "page {:>5}  {done}/{total}  {:?}/{:?}  attempts {}  {} ms{}",
                page.page,
                page.method,
                page.status,
                page.attempts,
                page.render_ms + page.ocr_ms,
                if page.warnings.is_empty() {
                    String::new()
                } else {
                    format!("  [{}]", page.warnings.join("; "))
                }
            );
        }));
    }
    let report = match converter
        .convert(&args.source, &args.destination, &options)
        .await
    {
        Ok(report) => report,
        Err(DocumentError::NoModel) => {
            eprintln!(
                "sven: no OCR model is available ({}).\n\
                 Add a provider that serves one (for example a brain server with \
                 DeepSeek-OCR-2, or another OpenAI-compatible service) and set `ocr.model`, \
                 or pass --model. `sven list-models` shows what can be used now.",
                unavailable.unwrap_or_default()
            );
            return Ok(EXIT_NO_MODEL);
        }
        Err(e) => return Err(e.into()),
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{}  {} pages: {} native, {} ocr, {} blank, {} reused, {} warning, {} failed  {:.1}s\nsha256 {}\nmanifest {}",
            report.output.display(),
            report.pages_total,
            report.pages_native,
            report.pages_ocr,
            report.pages_blank,
            report.pages_reused,
            report.pages_warning,
            report.pages_failed,
            report.elapsed_ms as f64 / 1000.0,
            report.sha256,
            report.manifest.display(),
        );
        for w in &report.warnings {
            eprintln!("warning: {w}");
        }
    }
    Ok(if report.pages_failed > 0 {
        EXIT_PARTIAL
    } else {
        0
    })
}
