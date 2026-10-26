// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::path::PathBuf;

use clap::{Args, Subcommand};

/// `sven document` subcommands.
#[derive(Subcommand, Debug)]
pub enum DocumentCommands {
    /// Convert a PDF to a Markdown file with an OCR model.
    ///
    /// Pages with a good native text layer keep it; the others are rendered
    /// and read by the OCR model (`--ocr` changes that). The model is any
    /// image-capable model sven can reach: `brain/deepseek-ai/DeepSeek-OCR-2`,
    /// `openrouter/qwen/qwen2.5-vl-72b-instruct`, an `ocr.models` alias or a
    /// loose name such as "deepseek ocr v2". A manifest beside the output
    /// (`<out>.manifest.json`) makes a rerun redo only missing or invalid pages.
    ///
    ///   sven document convert paper.pdf paper.md
    ///   sven document convert scan.pdf scan.md --model "deepseek ocr v2" --ocr always
    Convert(ConvertArgs),

    /// Character error rate of a conversion against a reference text.
    ///
    /// Whitespace is normalised on both sides; prints a number between 0 and
    /// about 1 (0 is identical).
    Score {
        /// Reference (ground truth) text file.
        reference: PathBuf,
        /// Text file to score, e.g. a converted Markdown file.
        hypothesis: PathBuf,
    },
}

#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// PDF to convert.
    pub source: PathBuf,
    /// Markdown file to write (published atomically).
    pub destination: PathBuf,
    /// OCR model: a spec, an `ocr.models` alias or a loose name. Default:
    /// `ocr.model`, else DeepSeek-OCR-2 on a provider that serves it.
    #[arg(long, short = 'm')]
    pub model: Option<String>,
    /// Pages to convert, 1-based: `1-3,7,10-`. Default: all.
    #[arg(long)]
    pub pages: Option<String>,
    /// Which pages go to the OCR model.
    #[arg(long, value_parser = ["auto", "always", "never"], default_value = "auto")]
    pub ocr: String,
    /// How pages are read: `auto` reads a page block by block from its layout
    /// (the model locates columns and articles, each block is read from a crop)
    /// when it looks like columns or its whole-page reading failed; `regions`
    /// does so for every OCR page; `off` reads every page whole.
    #[arg(long, value_parser = ["auto", "off", "regions"])]
    pub layout: Option<String>,
    /// Instruction for the layout pass.
    #[arg(long)]
    pub layout_prompt: Option<String>,
    /// Resolution the layout pass reads blocks at.
    #[arg(long)]
    pub region_dpi: Option<f32>,
    /// Model requests in flight at once per endpoint (not the server's batch size).
    #[arg(long)]
    pub concurrency: Option<usize>,
    /// Render resolution in dots per inch.
    #[arg(long)]
    pub dpi: Option<f32>,
    /// Output-token limit per page.
    #[arg(long)]
    pub max_tokens: Option<u32>,
    /// Instruction sent with each page image.
    #[arg(long)]
    pub prompt: Option<String>,
    /// Attempts per page; each retry renders at a higher resolution.
    #[arg(long, default_value_t = 3)]
    pub max_attempts: u32,
    /// Put `<!-- page: N -->` before each page.
    #[arg(long)]
    pub page_markers: bool,
    /// Remove first or last lines that repeat on most pages.
    #[arg(long)]
    pub dedupe_headers: bool,
    /// Ignore an earlier run's manifest and redo every page.
    #[arg(long)]
    pub no_resume: bool,
    /// Print the final report as JSON on stdout.
    #[arg(long)]
    pub json: bool,
    /// No per-page progress on stderr.
    #[arg(long, short = 'q')]
    pub quiet: bool,
}
