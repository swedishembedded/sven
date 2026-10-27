// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One page: render, ask the model, validate, retry, and for pages with
//! columns read block by block from the layout the model located.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures::StreamExt;
use sven_model::{
    CompletionRequest, ContentPart, Message, ModelProvider, ResponseEvent, SamplingParams,
};
use tokio::sync::Semaphore;

use crate::backend::{PdfSource, RenderedPage};
use crate::convert::{ConvertOptions, LayoutMode, OcrMode};
use crate::error::DocumentError;
use crate::layout::{merge_runs, reading_order, LayoutParser, Region};
use crate::limiter::{is_pushback, Limiter};
use crate::manifest::{sha256_hex, LayoutInfo, Method, PageRecord, Status};
use crate::quality::plausible_share;
use crate::validate::{check, clean, Problem};

/// Server pushbacks one request waits out before they count as failures.
const MAX_PUSHBACKS: u32 = 8;
/// Blocks of one page read at the same time; the limiter bounds the model
/// side, this bounds the crops waiting in memory.
const BLOCKS_AT_ONCE: usize = 8;
/// Output tokens a block may take per unit of page area (a full page of dense
/// newspaper print is about 10,000 tokens). A block that needs more than this is a model
/// inventing text for a picture, and the limit stops it sooner.
const BLOCK_TOKENS_PER_PAGE: f32 = 12000.0;
/// Fewest output tokens a block is allowed.
const MIN_BLOCK_TOKENS: u32 = 384;
/// A crop whose text has a smaller share of word-shaped tokens is not trusted.
const MIN_BLOCK_WORDS: f32 = 0.5;
/// Pages the model finds fewer columns on than this are read whole in `auto`,
/// not block by block.
const MIN_COLUMNS_FOR_CROPS: usize = 3;
/// Page images with at least this many columns...
const AUTO_LAYOUT_COLUMNS: usize = 2;
/// ...and at least this dark go through the layout pass in `auto` mode:
/// newspaper print is dense (0.13-0.23 measured), the rules of a register table
/// or a figure page are not (0.03-0.08).
const AUTO_LAYOUT_INK: f32 = 0.08;

pub(crate) struct PageContext {
    pub doc: Arc<dyn PdfSource>,
    pub providers: Vec<Arc<dyn ModelProvider>>,
    pub limiter: Arc<Limiter>,
    pub options: ConvertOptions,
    pub render_slots: Semaphore,
    pub parser: Arc<dyn LayoutParser>,
}

pub(crate) struct PageWork {
    /// 1-based.
    pub page: usize,
    pub method: Method,
    pub key: String,
    /// Native text is good enough to compare an OCR answer against.
    pub reference: bool,
}

/// A page rendered ahead of its turn at the model, with the time it took.
pub(crate) struct Prepared {
    pub image: Result<RenderedPage, DocumentError>,
    pub render_ms: u64,
}

/// What a request looks at: a whole page or a block of it.
#[derive(Clone, Copy)]
enum Target {
    Page(usize),
    Block(usize, [f32; 4]),
}

pub(crate) async fn render(
    ctx: &PageContext,
    index: usize,
    dpi: f32,
) -> (Result<RenderedPage, DocumentError>, u64) {
    render_target(ctx, Target::Page(index), dpi).await
}

async fn render_target(
    ctx: &PageContext,
    target: Target,
    dpi: f32,
) -> (Result<RenderedPage, DocumentError>, u64) {
    let _slot = ctx
        .render_slots
        .acquire()
        .await
        .expect("semaphore is never closed");
    let doc = Arc::clone(&ctx.doc);
    let started = Instant::now();
    let image = match tokio::task::spawn_blocking(move || match target {
        Target::Page(index) => doc.render(index, dpi),
        Target::Block(index, bbox) => doc.render_region(index, dpi, bbox),
    })
    .await
    {
        Ok(r) => r,
        Err(e) => Err(DocumentError::InvalidPdf(format!(
            "render worker failed: {e}"
        ))),
    };
    (image, started.elapsed().as_millis() as u64)
}

pub(crate) async fn process_page(
    ctx: &PageContext,
    work: PageWork,
    native: Option<String>,
    prepared: Option<Prepared>,
) -> PageRecord {
    let mut record = PageRecord {
        page: work.page,
        key: work.key.clone(),
        method: work.method,
        status: Status::Ok,
        model: None,
        attempts: 0,
        dpi: None,
        sha256: String::new(),
        text: String::new(),
        warnings: Vec::new(),
        layout: None,
        render_ms: 0,
        ocr_ms: 0,
        input_tokens: 0,
        output_tokens: 0,
    };
    match (work.method, prepared) {
        (Method::Ocr, Some(prepared)) => {
            record.model = Some(ctx.options.model_spec.clone());
            record.render_ms = prepared.render_ms;
            recognise(ctx, &work, native.as_deref(), prepared, &mut record).await;
        }
        _ => {
            record.text = native.as_deref().unwrap_or_default().trim().to_string();
            if record.text.is_empty() {
                record.status = Status::Warning;
                record.warnings = vec!["the page has no native text".into()];
            } else if !work.reference {
                record.status = Status::Warning;
                record.warnings = vec!["the native text layer looks unreliable".into()];
            }
        }
    }
    record.sha256 = sha256_hex(record.text.as_bytes());
    record
}

async fn recognise(
    ctx: &PageContext,
    work: &PageWork,
    native: Option<&str>,
    prepared: Prepared,
    record: &mut PageRecord,
) {
    let opts = &ctx.options;
    let (columns, ink) = prepared
        .image
        .as_ref()
        .map_or((1, 0.0), |i| (i.columns, i.ink));
    let layout_first = match opts.layout {
        LayoutMode::Off => false,
        LayoutMode::Regions => true,
        LayoutMode::Auto => columns >= AUTO_LAYOUT_COLUMNS && ink >= AUTO_LAYOUT_INK,
    };
    let reference = native.filter(|_| work.reference);
    let mut image = Some(prepared.image);
    let force = opts.layout == LayoutMode::Regions;
    if layout_first && read_by_layout(ctx, work, native, image.take().unwrap(), force, record).await
    {
        keep_text_layer_for_digits(opts, native, record);
        return;
    }
    let reading = read(
        ctx,
        Request {
            target: Target::Page(work.page - 1),
            prompt: &opts.prompt,
            reference,
            numbers_from: native,
            grounded: false,
        },
        image,
    )
    .await;
    absorb(record, &reading);
    settle(record, reading);
    // A page the whole-page request could not get right is the case the
    // layout pass exists for: columns that interleave or a loop in the second.
    let cut = record
        .warnings
        .iter()
        .any(|w| w == Problem::Cutoff.describe() || w == Problem::Repetition.describe());
    if opts.layout == LayoutMode::Auto && !layout_first && cut {
        let before = record.clone();
        record.warnings.clear();
        if !read_by_layout(
            ctx,
            work,
            native,
            render(ctx, work.page - 1, opts.dpi).await.0,
            true,
            record,
        )
        .await
        {
            *record = before;
        }
    }
    keep_text_layer_for_digits(opts, native, record);
}

/// In `auto`, a page whose recognised numbers still disagree with the page's
/// own text layer keeps the text layer: a misread digit in a register value is
/// worse than lost formatting. `--ocr always` asked for the recognised text.
fn keep_text_layer_for_digits(
    opts: &ConvertOptions,
    native: Option<&str>,
    record: &mut PageRecord,
) {
    let differs = record
        .warnings
        .iter()
        .any(|w| w == Problem::NumbersDiffer.describe());
    let layer = native.map(str::trim).filter(|t| !t.is_empty());
    if let (true, OcrMode::Auto, Some(layer)) = (differs, opts.ocr, layer) {
        record.method = Method::Native;
        record.text = layer.to_string();
        record.layout = None;
        record.status = Status::Warning;
        record.warnings = vec![
            "the recognised numbers differ from the text layer; the text layer was kept".into(),
        ];
    }
}

/// Records how the reading went on the page: a text with its warnings, or a
/// failure.
fn settle(record: &mut PageRecord, reading: Reading) {
    if reading.blank {
        record.method = Method::Blank;
        record.dpi = Some(reading.dpi);
        return;
    }
    record.dpi = Some(reading.dpi);
    record.warnings = reading
        .problems
        .iter()
        .map(|p| p.describe().to_string())
        .collect();
    match (reading.text.is_empty(), reading.error) {
        (false, _) => {
            record.status = if reading.problems.is_empty() {
                Status::Ok
            } else {
                Status::Warning
            };
            record.text = reading.text;
        }
        (true, Some(error)) => {
            record.warnings = vec![error];
            record.status = Status::Failed;
        }
        (true, None) => record.status = Status::Warning,
    }
}

fn absorb(record: &mut PageRecord, reading: &Reading) {
    record.attempts += reading.attempts;
    record.render_ms += reading.render_ms;
    record.ocr_ms += reading.ocr_ms;
    record.input_tokens += reading.input_tokens;
    record.output_tokens += reading.output_tokens;
}

/// Reads a page block by block. Returns `false` when the model located no
/// blocks and the caller should read the page whole.
async fn read_by_layout(
    ctx: &PageContext,
    work: &PageWork,
    native: Option<&str>,
    image: Result<RenderedPage, DocumentError>,
    force_crops: bool,
    record: &mut PageRecord,
) -> bool {
    let opts = &ctx.options;
    let index = work.page - 1;
    let grounding = read(
        ctx,
        Request {
            target: Target::Page(index),
            prompt: &opts.layout_prompt,
            reference: native.filter(|_| work.reference),
            numbers_from: native,
            grounded: true,
        },
        Some(image),
    )
    .await;
    absorb(record, &grounding);
    if grounding.blank {
        settle(record, grounding);
        return true;
    }
    let regions = ctx.parser.parse(&grounding.text);
    if regions.is_empty() {
        if grounding.text.is_empty() || grounding.text.contains("<|") {
            record
                .warnings
                .push("the model located no layout; reading the page whole".into());
            return false;
        }
        // An answer without any location is a plain reading of the page.
        let note = "the model does not locate blocks; the page was read whole";
        settle(record, grounding);
        record.warnings.push(note.into());
        return true;
    }
    let columns = crate::layout::column_count(&regions);
    if !force_crops && columns < MIN_COLUMNS_FOR_CROPS {
        // Not a newspaper after all (a page with figures): the whole-page
        // reading is the better one and the caller does it.
        return false;
    }
    let ordered: Vec<Region> = merge_runs(reading_order(regions))
        .into_iter()
        .filter(Region::is_textual)
        .collect();
    let total = ordered.len();
    let dpi = opts.region_dpi.max(opts.dpi);
    // Headlines and one-line blocks are read well enough at page resolution:
    // the layout pass's own text stands, and they cost no request. The bodies
    // go to crops, all rendered in one pass so the page's images are decoded
    // once rather than once per block.
    let wanted: Vec<Region> = ordered.iter().filter(|r| r.needs_crop()).cloned().collect();
    let boxes: Vec<[f32; 4]> = wanted.iter().map(|r| r.bbox).collect();
    let crops = render_blocks(ctx, index, dpi, boxes).await;
    let mut read_blocks = futures::stream::iter(wanted.into_iter().zip(crops))
        .map(|(region, (crop, render_ms))| async move {
            let reading = read(
                ctx,
                Request {
                    target: Target::Block(index, region.bbox),
                    prompt: &opts.prompt,
                    reference: None,
                    numbers_from: None,
                    grounded: false,
                },
                Some(crop),
            )
            .await;
            let mut reading = reading;
            reading.render_ms += render_ms;
            reading
        })
        .buffered(BLOCKS_AT_ONCE)
        .collect::<Vec<_>>()
        .await
        .into_iter();
    let mut parts = Vec::with_capacity(total);
    let mut degraded = 0;
    for region in &ordered {
        if !region.needs_crop() {
            if !region.text.trim().is_empty() {
                parts.push(heading(&region.label, region.text.clone()));
            }
            continue;
        }
        let reading = read_blocks.next().expect("one reading per cropped block");
        absorb(record, &reading);
        // A crop that looped, ran to the limit or is mostly non-words is a
        // model inventing text for a picture or a smudge: the layout pass's
        // own text for the block stands in, or nothing if it had none.
        let invented = reading.problems.contains(&Problem::Cutoff)
            || reading.problems.contains(&Problem::Repetition)
            || (!reading.text.is_empty() && plausible_share(&reading.text) < MIN_BLOCK_WORDS);
        let body = if reading.text.is_empty() || invented {
            region.text.clone()
        } else {
            reading.text.clone()
        };
        if invented || reading.text.is_empty() || !reading.problems.is_empty() {
            degraded += 1;
        }
        if !body.trim().is_empty() {
            parts.push(heading(&region.label, body));
        }
    }
    drop_repeats(&mut parts);
    record.text = parts.join("\n\n");
    record.dpi = Some(dpi);
    record.layout = Some(LayoutInfo {
        regions: total,
        columns,
        degraded,
    });
    if degraded > 0 {
        record.status = Status::Warning;
        record.warnings.push(format!(
            "{degraded} of {total} blocks did not read cleanly; the layout pass's text or a failed check stands"
        ));
    }
    if record.text.is_empty() {
        record.status = Status::Warning;
        record.warnings.push("no block produced text".into());
    }
    true
}

/// Crops of neighbouring blocks overlap where the model's boxes do, so the
/// same text can come out twice: a line on a seam, or a whole paragraph that
/// two boxes both cover. A paragraph (or, failing that, a first line) that
/// repeats one of the two blocks before it, within a few misread characters,
/// is dropped from the later block.
fn drop_repeats(parts: &mut Vec<String>) {
    const SIMILAR: f64 = 0.25;
    const MIN_PARAGRAPH_CHARS: usize = 40;
    const SIMILAR_LINE: f64 = 0.15;
    const LOOK_BACK: usize = 2;
    let squash = |s: &str| -> String {
        // Hyphenated line ends join: "stu-\ndent" is "student".
        let joined = s.replace("-\n", "");
        joined
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let near = |a: &str, b: &str| {
        a == b
            || crate::score::character_error_rate(a, b).is_some_and(|c| c <= SIMILAR)
            || (a.len().min(b.len()) >= MIN_PARAGRAPH_CHARS && (a.contains(b) || b.contains(a)))
    };
    for i in 1..parts.len() {
        let earlier: Vec<String> = parts[i.saturating_sub(LOOK_BACK)..i]
            .iter()
            .flat_map(|p| p.split("\n\n"))
            .map(squash)
            .filter(|p| p.chars().count() >= MIN_PARAGRAPH_CHARS)
            .collect();
        let kept: Vec<&str> = parts[i]
            .split("\n\n")
            .filter(|para| {
                let sq = squash(para);
                let heading = para.trim_start().starts_with('#');
                heading
                    || sq.chars().count() < MIN_PARAGRAPH_CHARS
                    || !earlier.iter().any(|e| near(e, &sq))
            })
            .collect();
        let mut text = kept.join("\n\n");
        // The seam: the first lines of this block are the end of the block
        // before (a crop that began a line or two too early).
        if let Some(end) = repeated_head(&parts[i - 1], &text, SIMILAR_LINE) {
            text = text
                .lines()
                .skip(end)
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
        }
        parts[i] = text;
    }
    parts.retain(|p| !p.trim().is_empty());
}

/// Fewest characters a repeated head must have to be told from coincidence.
const MIN_HEAD_CHARS: usize = 20;
/// Fewest characters of a repeated single line that is exactly the end of the
/// block before.
const MIN_EXACT_HEAD_CHARS: usize = 6;
/// Most lines of a block that can repeat the end of the one before.
const MAX_HEAD_LINES: usize = 5;

/// How many leading lines of `next` repeat the end of `previous` (0 if none):
/// the most that read, joined, like the same number of characters at the end
/// of `previous`, within `similar` edit distance.
fn repeated_head(previous: &str, next: &str, similar: f64) -> Option<usize> {
    let squash = |s: &str| -> String {
        s.replace("-\n", "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    if next.trim_start().starts_with('#') || previous.trim_start().starts_with('#') {
        return None;
    }
    let tail = squash(previous);
    let lines: Vec<&str> = next.lines().collect();
    let mut found = None;
    let mut head = String::new();
    for (n, line) in lines.iter().enumerate().take(MAX_HEAD_LINES) {
        head = format!("{head}\n{line}");
        let head_sq = squash(&head);
        let len = head_sq.chars().count();
        if len < MIN_EXACT_HEAD_CHARS || len > tail.chars().count() {
            continue;
        }
        let end: String = tail.chars().skip(tail.chars().count() - len).collect();
        let same = head_sq == end
            || len >= MIN_HEAD_CHARS
                && crate::score::character_error_rate(&end, &head_sq).is_some_and(|c| c <= similar);
        if same {
            found = Some(n + 1);
        }
    }
    found
}

/// The crops of `boxes` on page `index`, each with its share of the time.
async fn render_blocks(
    ctx: &PageContext,
    index: usize,
    dpi: f32,
    boxes: Vec<[f32; 4]>,
) -> Vec<(Result<RenderedPage, DocumentError>, u64)> {
    let n = boxes.len();
    let _slot = ctx
        .render_slots
        .acquire()
        .await
        .expect("semaphore is never closed");
    let doc = Arc::clone(&ctx.doc);
    let started = Instant::now();
    let crops = tokio::task::spawn_blocking(move || doc.render_regions(index, dpi, &boxes))
        .await
        .unwrap_or_default();
    let each = started.elapsed().as_millis() as u64 / n.max(1) as u64;
    let mut crops: Vec<_> = crops.into_iter().map(|c| (c, each)).collect();
    while crops.len() < n {
        let failed = DocumentError::InvalidPdf("render worker failed".into());
        crops.push((Err(failed), each));
    }
    crops
}

/// A headline block read from a crop comes back as running text; mark it.
fn heading(label: &str, body: String) -> String {
    let marker = match label {
        "title" => "# ",
        "sub_title" => "## ",
        _ => return body,
    };
    if body.trim_start().starts_with('#') {
        body
    } else {
        format!("{marker}{}", body.trim())
    }
}

// ── One reading: a request with validation and retries ──────────────────────

struct Request<'a> {
    target: Target,
    prompt: &'a str,
    /// Native text to compare the answer with, when it is trustworthy.
    reference: Option<&'a str>,
    /// Native text whose numbers the answer should reproduce, trustworthy or
    /// not: digits survive in a text layer that is wrong in its letters.
    numbers_from: Option<&'a str>,
    /// The answer is expected to carry layout markers.
    grounded: bool,
}

struct Reading {
    text: String,
    problems: Vec<Problem>,
    error: Option<String>,
    blank: bool,
    attempts: u32,
    dpi: f32,
    render_ms: u64,
    ocr_ms: u64,
    input_tokens: u64,
    output_tokens: u64,
}

/// Ask for `request`, validate, and retry at a higher resolution while the
/// budget lasts. `first` is an image already rendered at the base resolution.
async fn read(
    ctx: &PageContext,
    request: Request<'_>,
    first: Option<Result<RenderedPage, DocumentError>>,
) -> Reading {
    let opts = &ctx.options;
    let base_dpi = match request.target {
        Target::Page(_) => opts.dpi,
        Target::Block(..) => opts.region_dpi.max(opts.dpi),
    };
    let mut reading = Reading {
        text: String::new(),
        problems: Vec::new(),
        error: None,
        blank: false,
        attempts: 0,
        dpi: base_dpi,
        render_ms: 0,
        ocr_ms: 0,
        input_tokens: 0,
        output_tokens: 0,
    };
    let max_tokens = match request.target {
        Target::Page(_) => opts.max_tokens,
        Target::Block(_, b) => {
            let area = (b[2] - b[0]) * (b[3] - b[1]);
            ((area * BLOCK_TOKENS_PER_PAGE) as u32).clamp(MIN_BLOCK_TOKENS, opts.max_tokens)
        }
    };
    let mut first = first;
    let mut best: Option<(Vec<Problem>, String, f32)> = None;
    let mut pushbacks = 0;
    let mut attempt = 0;
    while attempt < opts.max_attempts.max(1) {
        attempt += 1;
        reading.attempts = attempt;
        let dpi = base_dpi * (1.0 + 0.5 * (attempt - 1) as f32);
        let image = match first.take() {
            Some(image) => image,
            None => {
                let (image, ms) = render_target(ctx, request.target, dpi).await;
                reading.render_ms += ms;
                image
            }
        };
        let image = match image {
            Ok(i) => i,
            Err(e) => {
                reading.error = Some(format!("render failed: {e}"));
                break;
            }
        };
        if image.blank {
            reading.blank = true;
            reading.dpi = dpi;
            return reading;
        }
        // The same image is asked again after a pushback, without spending an attempt.
        let answer = loop {
            let slot = ctx.limiter.acquire().await;
            let started = Instant::now();
            let outcome = ask(ctx, slot.endpoint(), &image, request.prompt, max_tokens).await;
            reading.ocr_ms += started.elapsed().as_millis() as u64;
            match outcome {
                Ok(a) => {
                    slot.succeeded();
                    break Ok(a);
                }
                Err(e) if is_pushback(&e) && pushbacks < MAX_PUSHBACKS => {
                    slot.overloaded();
                    drop(slot);
                    pushbacks += 1;
                    tokio::time::sleep(Duration::from_millis(500 << pushbacks.min(5))).await;
                }
                Err(e) => break Err(e),
            }
        };
        match answer {
            Ok(answer) => {
                reading.input_tokens += answer.input_tokens;
                reading.output_tokens += answer.output_tokens;
                let text = clean(&answer.text);
                let mut problems = check(
                    &text,
                    answer.cutoff,
                    request.reference,
                    request.numbers_from,
                );
                if request.grounded {
                    problems.retain(|p| *p != Problem::SpecialTokens);
                }
                reading.error = None;
                if problems.is_empty() {
                    reading.text = text;
                    reading.problems.clear();
                    reading.dpi = dpi;
                    return reading;
                }
                let better = best.as_ref().is_none_or(|(p, t, _)| {
                    (problems.len(), std::cmp::Reverse(text.len()))
                        < (p.len(), std::cmp::Reverse(t.len()))
                });
                // A crop that loops or runs on is a model inventing text for a
                // picture; a larger crop of the same picture will not help.
                let hopeless = matches!(request.target, Target::Block(..))
                    && problems
                        .iter()
                        .any(|p| matches!(p, Problem::Cutoff | Problem::Repetition));
                if better {
                    best = Some((problems, text, dpi));
                }
                if hopeless {
                    break;
                }
            }
            Err(e) => {
                reading.error = Some(e);
                tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
            }
        }
    }
    if let Some((problems, text, dpi)) = best {
        reading.problems = problems;
        reading.text = text;
        reading.dpi = dpi;
        reading.error = None;
    }
    reading
}

struct Answer {
    text: String,
    cutoff: bool,
    input_tokens: u64,
    output_tokens: u64,
}

async fn ask(
    ctx: &PageContext,
    endpoint: usize,
    page: &RenderedPage,
    prompt: &str,
    max_tokens: u32,
) -> Result<Answer, String> {
    let url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&page.png)
    );
    let request = CompletionRequest {
        messages: vec![Message::user_with_parts(vec![
            ContentPart::image(url),
            ContentPart::text(prompt.to_string()),
        ])],
        stream: true,
        max_output_tokens_override: Some(max_tokens),
        sampling: SamplingParams {
            temperature: Some(0.0),
            ..Default::default()
        },
        ..Default::default()
    };
    let exchange = async {
        let mut stream = ctx.providers[endpoint]
            .complete(request)
            .await
            .map_err(|e| format!("model request failed: {e:#}"))?;
        let mut answer = Answer {
            text: String::new(),
            cutoff: false,
            input_tokens: 0,
            output_tokens: 0,
        };
        while let Some(event) = stream.next().await {
            match event.map_err(|e| format!("model stream failed: {e:#}"))? {
                ResponseEvent::TextDelta(t) => answer.text.push_str(&t),
                ResponseEvent::MaxTokens => answer.cutoff = true,
                ResponseEvent::Usage {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    answer.input_tokens += u64::from(input_tokens);
                    answer.output_tokens += u64::from(output_tokens);
                }
                ResponseEvent::Error(e) => return Err(format!("model error: {e}")),
                _ => {}
            }
        }
        Ok(answer)
    };
    tokio::time::timeout(ctx.options.request_timeout, exchange)
        .await
        .map_err(|_| "model request timed out".to_string())?
}

#[cfg(test)]
mod tests {
    use super::drop_repeats;

    const OPENING: &str =
        "Atlanta, May 17 - Thousands of student youth, joined by hundreds of adults, \
                           marched in this city's biggest demonstration against segregation.";

    /// The 1960 newspaper's first page: two crops both covered the opening
    /// paragraph, the second with line breaks and a misread letter.
    #[test]
    fn a_paragraph_two_crops_both_cover_is_kept_once() {
        let wrapped = "Atlanta, May 17 - Thousands of stu-\ndent youth, joined by hundreds of adults,\nmarched in this city's biggest demon-\nstration against segregat1on.";
        let mut parts = vec![
            format!("# HEADLINE\n\n{OPENING}"),
            format!(
                "{wrapped}\n\nA second paragraph that only the later crop has, about the march."
            ),
        ];
        // The headline stays; the repeated paragraph goes from the later block.
        drop_repeats(&mut parts);
        let all = parts.join("\n\n");
        assert_eq!(all.matches("Atlanta, May 17").count(), 1, "{all}");
        assert!(all.contains("A second paragraph"));
    }

    #[test]
    fn a_crop_that_began_a_few_lines_early_does_not_repeat_the_block_before() {
        let before = "The capitalist class of the South rules mainly by the trick of white \
                      supremacy, that is by keeping the Negro down and telling the poor white he is \
                      superior to the Negro.";
        let after = "acy, that is by keeping the Negro down\nand telling the poor white he is su-\nperior to the Negro.\nUnder this system no Negro can be\nallowed to hold office.";
        let mut parts = vec![before.to_string(), after.to_string()];
        drop_repeats(&mut parts);
        assert_eq!(
            parts[1],
            "Under this system no Negro can be\nallowed to hold office."
        );
    }

    #[test]
    fn short_repeated_lines_and_distinct_paragraphs_are_left_alone() {
        let mut parts = vec![
            "Yes.\n\nThe committee approved the budget for the new library on Tuesday.".to_string(),
            "Yes.\n\nThe regional grant was increased by three percent this year, it said."
                .to_string(),
        ];
        drop_repeats(&mut parts);
        assert_eq!(parts[1].matches("Yes.").count(), 1);
        assert!(parts[1].contains("regional grant"));
    }
}
