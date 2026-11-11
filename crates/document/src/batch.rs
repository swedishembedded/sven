// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Several pages in one request.
//!
//! A model with a large output allowance can read a handful of pages in one
//! request, for fewer round trips (not necessarily fewer tokens: every page
//! image is still billed). It is off unless chosen: whether a model reads
//! pages together as well as apart is measured per model, not assumed. The request names each
//! page and asks for its reading to begin with that page's separator line
//! ([`batch_separator`]); the answer is split there and each page is checked
//! on its own exactly as a page read alone. A page whose part does not pass
//! is read again alone, with the retries a page always gets, so batching can
//! make a run cheaper and never worse.
//!
//! Whether the model keeps the pages apart is learnt from the first batch,
//! which goes alone: until one answer has split, no other batch is sent. A
//! model that does not (a dedicated OCR model that ignores instructions) has
//! then cost one request, and the rest of the run reads a page per request.
//!
//! Swedish Embedded AB implements cost-efficient OCR pipelines for its
//! clients. If your team needs expertise in reading large documents with
//! hosted vision models then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use futures::future::join_all;
use sven_model::ModelProvider;

use crate::manifest::{sha256_hex, Method, PageRecord, Status};
use crate::page::{layout_first, new_record, process_page, PageContext, PageWork, Prepared};
use crate::request::ask_patiently;
use crate::validate::{check, clean};

/// Most pages one request carries: a batch that does not
/// split costs at most this many pages' worth of tokens.
pub const MAX_PAGES_PER_REQUEST: usize = 8;

/// The line that begins the reading of page `page` (1-based) in the answer
/// to a request that carries several pages.
#[must_use]
pub fn batch_separator(page: usize) -> String {
    format!("=== PAGE {page} ===")
}

/// What a run knows about reading pages together.
pub(crate) struct Batching {
    /// Pages per request; 1 reads every page alone.
    pub size: usize,
    /// `None` until an answer to a batch has split (or failed to);
    /// held while the first batch is out, so no second one goes before it.
    trust: tokio::sync::Mutex<Option<bool>>,
    batched: AtomicUsize,
    notes: Mutex<Vec<String>>,
}

impl Batching {
    pub fn new(size: usize) -> Self {
        Self {
            size: size.clamp(1, MAX_PAGES_PER_REQUEST),
            trust: tokio::sync::Mutex::new(None),
            batched: AtomicUsize::new(0),
            notes: Mutex::new(Vec::new()),
        }
    }

    /// Pages accepted from a request that carried others.
    pub fn pages_batched(&self) -> usize {
        self.batched.load(Ordering::SeqCst)
    }

    /// Run-level notes, for the report.
    pub fn notes(&self) -> Vec<String> {
        self.notes.lock().unwrap().clone()
    }
}

/// The pages per request a run uses: the `chosen` count, but never more
/// page allowances (`max_tokens`) than fit in the smallest output limit the
/// endpoints' listings state, since a batch cut off at that limit is wasted.
/// A listing that states no limit leaves the choice as it is.
pub(crate) fn fitted_size(
    chosen: usize,
    providers: &[std::sync::Arc<dyn ModelProvider>],
    max_tokens: u32,
) -> usize {
    providers
        .iter()
        .filter_map(|p| p.catalog_max_output_tokens())
        .map(|limit| (limit / max_tokens.max(1)) as usize)
        .fold(chosen, usize::min)
        .clamp(1, MAX_PAGES_PER_REQUEST)
}

/// One page waiting at the model: its work, its native text, its rendering.
pub(crate) type Pending = (PageWork, Option<String>, Option<Prepared>);

/// The pages of `chunk` settled: those that can be read whole go to the model
/// together when the run batches, every other page (native, blank, read by
/// its layout, failed to render, or let down by its batch) as it always is.
pub(crate) async fn process_chunk(ctx: &PageContext, chunk: Vec<Pending>) -> Vec<PageRecord> {
    let at_model = chunk.iter().filter(|(_, _, p)| p.is_some()).count();
    let (mut together, alone): (Vec<Pending>, Vec<Pending>) =
        chunk.into_iter().partition(|(work, _, prepared)| {
            work.method == Method::Ocr
                && prepared.as_ref().is_some_and(|p| {
                    p.image
                        .as_ref()
                        .is_ok_and(|image| !image.blank && !layout_first(&ctx.options, image))
                })
        });
    let mut records = Vec::new();
    let mut retry = alone;
    if ctx.batching.size > 1 && together.len() > 1 {
        together.sort_by_key(|(work, _, _)| work.page);
        let (accepted, rest) = read_together(ctx, together).await;
        records = accepted;
        retry.extend(rest);
    } else {
        retry.extend(together);
    }
    let alone = retry
        .into_iter()
        .map(|(work, native, prepared)| process_page(ctx, work, native, prepared));
    records.extend(join_all(alone).await);
    for _ in 0..at_model {
        ctx.limiter.page_done();
    }
    records
}

/// Reads `pages` in one request when the model is known or about to be
/// learnt to keep them apart. Answers the pages accepted and those to read
/// alone.
async fn read_together(ctx: &PageContext, pages: Vec<Pending>) -> (Vec<PageRecord>, Vec<Pending>) {
    let mut trust = ctx.batching.trust.lock().await;
    match *trust {
        Some(false) => (Vec::new(), pages),
        Some(true) => {
            // Known to work: batches go side by side.
            drop(trust);
            let (accepted, rest, _) = ask_together(ctx, pages).await;
            (accepted, rest)
        }
        None => {
            // The first batch keeps the lock, so the others wait for it.
            let (accepted, rest, split) = ask_together(ctx, pages).await;
            *trust = Some(split);
            if !split {
                ctx.batching.notes.lock().unwrap().push(
                    "the model did not keep the pages of one request apart; the run read one \
                     page per request from then on"
                        .into(),
                );
            }
            (accepted, rest)
        }
    }
}

/// One request for all of `pages`. Answers the accepted records, the pages
/// to read alone, and whether the answer split into one part per page.
async fn ask_together(
    ctx: &PageContext,
    pages: Vec<Pending>,
) -> (Vec<PageRecord>, Vec<Pending>, bool) {
    let opts = &ctx.options;
    let numbers: Vec<usize> = pages.iter().map(|(work, _, _)| work.page).collect();
    let prompt = batch_prompt(&opts.prompt, &numbers);
    let images: Vec<_> = pages
        .iter()
        .filter_map(|(_, _, prepared)| prepared.as_ref()?.image.as_ref().ok())
        .collect();
    let budget = opts.max_tokens.saturating_mul(pages.len() as u32);
    let (answer, ocr_ms) = ask_patiently(ctx, &images, &prompt, budget).await;
    let parts = match &answer {
        // A cut-off answer lost its last page and maybe more; all go alone.
        Ok(answer) if !answer.cutoff => split(&answer.text, &numbers),
        _ => None,
    };
    let (Some(parts), Ok(answer)) = (parts, answer) else {
        return (Vec::new(), pages, false);
    };
    let count = pages.len() as u64;
    let share = |total: u64| total / count;
    let mut accepted = Vec::new();
    let mut rest = Vec::new();
    for ((work, native, prepared), part) in pages.into_iter().zip(parts) {
        let text = clean(&part);
        let reference = native.as_deref().filter(|_| work.reference);
        if !check(&text, false, reference, native.as_deref()).is_empty() {
            rest.push((work, native, prepared));
            continue;
        }
        let mut record = new_record(&work);
        record.model = Some(opts.model_spec.clone());
        record.attempts = 1;
        record.dpi = Some(opts.dpi);
        record.render_ms = prepared.map_or(0, |p| p.render_ms);
        record.ocr_ms = ocr_ms;
        record.input_tokens = share(answer.input_tokens);
        record.output_tokens = share(answer.output_tokens);
        record.pages_in_request = Some(numbers.len());
        record.status = Status::Ok;
        record.sha256 = sha256_hex(text.as_bytes());
        record.text = text;
        accepted.push(record);
    }
    ctx.batching
        .batched
        .fetch_add(accepted.len(), Ordering::SeqCst);
    (accepted, rest, true)
}

fn batch_prompt(prompt: &str, pages: &[usize]) -> String {
    let separators: Vec<String> = pages.iter().map(|p| batch_separator(*p)).collect();
    format!(
        "{prompt}\n\nThe {} images are pages of one document, in this order. Read each page \
         on its own and begin its reading with its line below, exactly as written; write \
         nothing before the first one.\n{}",
        pages.len(),
        separators.join("\n")
    )
}

/// `text` cut at the separators of `pages`, one part per page, or `None`
/// when they are not all there, once each, in order, with nothing before the
/// first.
fn split(text: &str, pages: &[usize]) -> Option<Vec<String>> {
    let mut parts: Vec<String> = Vec::with_capacity(pages.len());
    let mut next = pages.iter();
    let mut expected = next.next();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("=== PAGE ") && trimmed.ends_with(" ===") {
            match expected {
                Some(page) if trimmed == batch_separator(*page) => {
                    parts.push(String::new());
                    expected = next.next();
                    continue;
                }
                _ => return None,
            }
        }
        match parts.last_mut() {
            Some(part) => {
                part.push_str(line);
                part.push('\n');
            }
            None if trimmed.is_empty() => {}
            None => return None,
        }
    }
    (parts.len() == pages.len()).then_some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_splits_only_at_every_separator_once_and_in_order() {
        let pages = [3, 4];
        let ok = "\n=== PAGE 3 ===\nthree\n=== PAGE 4 ===\nfour\n";
        assert_eq!(split(ok, &pages).unwrap(), ["three\n", "four\n"]);
        for broken in [
            "=== PAGE 3 ===\nthree\n",                                // one missing
            "=== PAGE 4 ===\nfour\n=== PAGE 3 ===\nthree\n",          // out of order
            "preamble\n=== PAGE 3 ===\nthree\n=== PAGE 4 ===\nfour",  // text before
            "=== PAGE 3 ===\na\n=== PAGE 3 ===\nb\n=== PAGE 4 ===\n", // twice
            "three and four together",                                // no separators
        ] {
            assert!(split(broken, &pages).is_none(), "{broken}");
        }
    }
}
