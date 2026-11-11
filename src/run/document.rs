// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;

use sven_bootstrap::{Config, ConfigOcrModels};
use sven_document::{
    character_error_rate, ConvertOptions, Converter, DocumentError, OcrMode, StandardBackend,
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
        // Dispatched by `main` before anything else starts; see `worker`.
        DocumentCommands::Worker(_) => {
            anyhow::bail!("the document worker is started by sven itself")
        }
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
    if let Some(v) = &args.layout {
        options.layout = v.parse().map_err(anyhow::Error::msg)?;
    }
    if let Some(v) = &args.layout_prompt {
        options.layout_prompt = v.clone();
    }
    if let Some(v) = args.region_dpi {
        options.region_dpi = v;
    }
    if let Some(v) = args.concurrency {
        options.concurrency = v;
    }
    if let Some(v) = args.dpi {
        options.dpi = v;
    }
    if let Some(v) = args.max_tokens {
        options.max_tokens = v;
    }
    if let Some(v) = args.pages_per_request {
        options.pages_per_request = v;
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

    let mut converter = Converter::new(Arc::new(StandardBackend), providers);
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
            eprintln!("{}", no_model_text(&unavailable.unwrap_or_default()));
            return Ok(EXIT_NO_MODEL);
        }
        Err(e) => return Err(e.into()),
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{}  {} pages: {} native, {} ocr ({} read with others), {} blank, {} reused, {} warning, {} failed  {:.1}s\nrequests: peak {} in flight, backend idle {} ms, per endpoint {:?}\nsha256 {}\nmanifest {}",
            report.output.display(),
            report.pages_total,
            report.pages_native,
            report.pages_ocr,
            report.pages_batched,
            report.pages_blank,
            report.pages_reused,
            report.pages_warning,
            report.pages_failed,
            report.elapsed_ms as f64 / 1000.0,
            report.max_in_flight,
            report.backend_idle_ms,
            report.requests_per_endpoint,
            report.sha256,
            report.manifest.display(),
        );
        for note in &report.notes {
            eprintln!("note: {note}");
        }
        for w in &report.warnings {
            eprintln!("warning: {w}");
        }
    }
    if let (Some(reason), true) = (&unavailable, report.pages_failed > 0) {
        eprintln!("{}", no_model_text(reason));
    }
    Ok(if report.pages_failed > 0 {
        EXIT_PARTIAL
    } else {
        0
    })
}

fn no_model_text(reason: &str) -> String {
    format!(
        "sven: no OCR model is available ({reason}).\n\
         Add a provider that serves one (for example a brain server with \
         DeepSeek-OCR-2, or another OpenAI-compatible service) and set `ocr.model`, \
         or pass --model. `sven list-models` shows what can be used now."
    )
}

/// `sven document worker`: limits itself, then serves one document over
/// stdin and stdout ([`sven_document::serve_worker`]). Runs before any
/// runtime, log or configuration exists, so nothing but frames reaches
/// stdout and nothing outside its confinement is touched. The exit status is
/// 0 when its input ended, 1 when it could not limit itself or serve.
pub(crate) fn worker(args: &crate::cli::WorkerArgs) -> i32 {
    if let Err(e) = limit_self(args.max_memory_mb, args.max_cpu_seconds) {
        eprintln!("sven document worker: {e}");
        return 1;
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match sven_document::serve_worker(&mut stdin.lock(), &mut stdout.lock(), &StandardBackend) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sven document worker: {e}");
            1
        }
    }
}

/// Hard limits on this process's address space and processor time, both
/// soft and hard, so neither can be raised again; never above the limits it
/// was started with.
fn limit_self(memory_mb: u64, cpu_seconds: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let set = |resource, wanted: u64| -> std::io::Result<()> {
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: getrlimit and setrlimit only read and write the struct
            // passed by pointer, which lives for the call.
            unsafe {
                if libc::getrlimit(resource, &mut current) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let limit: libc::rlim_t = std::cmp::min(wanted, current.rlim_max);
                let new = libc::rlimit {
                    rlim_cur: limit,
                    rlim_max: limit,
                };
                if libc::setrlimit(resource, &new) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        };
        set(libc::RLIMIT_AS, memory_mb.saturating_mul(1024 * 1024))?;
        set(libc::RLIMIT_CPU, cpu_seconds)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (memory_mb, cpu_seconds);
        Err(std::io::Error::other(
            "a document worker limits itself only on Unix",
        ))
    }
}
