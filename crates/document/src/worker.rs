// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reading a document someone else gave in a process of its own.
//!
//! Parsing and rendering a PDF is the part of a conversion that reads
//! hostile input: a crafted file can make a renderer loop, allocate without
//! end or hit a bug in a parser. [`IsolatedBackend`] keeps that out of the
//! process that holds the session, its keys and its connection to the model:
//! the document's bytes go to a worker process over a pipe, and only page
//! images, text and the outline come back. The worker is whatever the
//! caller's [`WorkerSpawner`] starts (the `sven document worker` command,
//! confined by the caller: no network, no file it could write, hard memory
//! and CPU limits it sets on itself before it reads a byte).
//!
//! Every request has a deadline. A worker that misses one, exits or answers
//! nonsense is killed and replaced, and the request fails as that page's
//! error, so one pathological page costs its own reading and nothing else.
//! The worker never sees a path, a key or the network; the parent never
//! parses the document.
//!
//! Frames, both ways: a 4-byte big-endian length and a JSON header, then a
//! 4-byte big-endian length and a binary payload (the document, a PNG, a JSON
//! list). Both are bounded on read.
//!
//! Swedish Embedded AB implements solutions for processing untrusted files
//! safely in multi-tenant services for its clients. If your team needs
//! expertise in sandboxed document processing then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::backend::{DocumentBackend, DocumentSource, RenderedPage};
use crate::error::DocumentError;
use crate::locate::OutlineEntry;

/// The largest JSON header of a frame.
const MAX_HEADER_BYTES: usize = 1 << 20;
/// The largest payload of a frame: a document sent to a worker, or what one
/// sends back.
pub const MAX_PAYLOAD_BYTES: usize = 512 << 20;

fn write_frame(out: &mut dyn Write, header: &Value, payload: &[u8]) -> io::Result<()> {
    let header = serde_json::to_vec(header)?;
    if header.len() > MAX_HEADER_BYTES || payload.len() > MAX_PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame too large",
        ));
    }
    out.write_all(&(header.len() as u32).to_be_bytes())?;
    out.write_all(&header)?;
    out.write_all(&(payload.len() as u32).to_be_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

fn read_length(input: &mut dyn Read, max: usize) -> io::Result<Option<usize>> {
    let mut length = [0u8; 4];
    match input.read_exact(&mut length) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    Ok(Some(length))
}

/// One frame, or `None` at a clean end of input.
fn read_frame(input: &mut dyn Read) -> io::Result<Option<(Value, Vec<u8>)>> {
    let Some(length) = read_length(input, MAX_HEADER_BYTES)? else {
        return Ok(None);
    };
    let mut header = vec![0u8; length];
    input.read_exact(&mut header)?;
    let header: Value = serde_json::from_slice(&header)?;
    let length = read_length(input, MAX_PAYLOAD_BYTES)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "frame without payload"))?;
    let mut payload = vec![0u8; length];
    input.read_exact(&mut payload)?;
    Ok(Some((header, payload)))
}

fn error_header(e: &DocumentError) -> Value {
    let kind = match e {
        DocumentError::Encrypted => "encrypted",
        DocumentError::Empty => "empty",
        DocumentError::Unsupported(_) => "unsupported",
        _ => "invalid",
    };
    json!({"error": e.to_string(), "kind": kind})
}

fn error_of(header: &Value) -> Option<DocumentError> {
    let message = header.get("error")?.as_str().unwrap_or("the worker failed");
    Some(match header.get("kind").and_then(Value::as_str) {
        Some("encrypted") => DocumentError::Encrypted,
        Some("empty") => DocumentError::Empty,
        Some("unsupported") => DocumentError::Unsupported(message.to_string()),
        _ => DocumentError::Invalid(message.to_string()),
    })
}

fn rendered_header(page: &RenderedPage) -> Value {
    json!({"width": page.width, "height": page.height, "blank": page.blank,
           "columns": page.columns, "ink": page.ink})
}

fn rendered_of(header: &Value, png: Vec<u8>) -> Result<RenderedPage, DocumentError> {
    if let Some(e) = error_of(header) {
        return Err(e);
    }
    let number = |key: &str| header.get(key).and_then(Value::as_u64);
    let bad = || DocumentError::Invalid("the worker answered a page it did not describe".into());
    Ok(RenderedPage {
        png,
        width: u32::try_from(number("width").ok_or_else(bad)?).map_err(|_| bad())?,
        height: u32::try_from(number("height").ok_or_else(bad)?).map_err(|_| bad())?,
        blank: header
            .get("blank")
            .and_then(Value::as_bool)
            .ok_or_else(bad)?,
        columns: usize::try_from(number("columns").ok_or_else(bad)?).map_err(|_| bad())?,
        ink: header.get("ink").and_then(Value::as_f64).ok_or_else(bad)? as f32,
    })
}

fn boxes_of(header: &Value) -> Option<Vec<[f32; 4]>> {
    header
        .get("boxes")?
        .as_array()?
        .iter()
        .map(|b| {
            let b = b.as_array().filter(|b| b.len() == 4)?;
            let mut out = [0f32; 4];
            for (slot, v) in out.iter_mut().zip(b) {
                *slot = v.as_f64()? as f32;
            }
            Some(out)
        })
        .collect()
}

/// The worker's side: open the document the first frame carries with
/// `backend`, then answer requests until the input ends. A request it cannot
/// read ends the worker; the parent replaces it.
///
/// # Errors
///
/// The pipe failed, or a frame was malformed.
pub fn serve(
    input: &mut dyn Read,
    output: &mut dyn Write,
    backend: &dyn DocumentBackend,
) -> io::Result<()> {
    let Some((header, document)) = read_frame(input)? else {
        return Ok(());
    };
    if header.get("op").and_then(Value::as_str) != Some("open") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the first request must open a document",
        ));
    }
    let source = match backend.open(Arc::new(document)) {
        Ok(source) => {
            write_frame(output, &json!({"pages": source.page_count()}), &[])?;
            source
        }
        Err(e) => return write_frame(output, &error_header(&e), &[]),
    };
    while let Some((header, _)) = read_frame(input)? {
        let index = header
            .get("index")
            .and_then(Value::as_u64)
            .map(|i| i as usize);
        let dpi = header.get("dpi").and_then(Value::as_f64).map(|d| d as f32);
        match (header.get("op").and_then(Value::as_str), index, dpi) {
            (Some("native_text"), _, _) => {
                write_frame(
                    output,
                    &json!({}),
                    &serde_json::to_vec(&source.native_text())?,
                )?;
            }
            (Some("outline"), _, _) => match source.outline() {
                Ok(entries) => write_frame(output, &json!({}), &serde_json::to_vec(&entries)?)?,
                Err(e) => write_frame(output, &error_header(&e), &[])?,
            },
            (Some("render"), Some(index), Some(dpi)) => match source.render(index, dpi) {
                Ok(page) => write_frame(output, &rendered_header(&page), &page.png)?,
                Err(e) => write_frame(output, &error_header(&e), &[])?,
            },
            (Some("render_regions"), Some(index), Some(dpi)) => {
                let boxes = boxes_of(&header).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "regions without boxes")
                })?;
                let pages = source.render_regions(index, dpi, &boxes);
                write_frame(output, &json!({"count": pages.len()}), &[])?;
                for page in pages {
                    match page {
                        Ok(page) => write_frame(output, &rendered_header(&page), &page.png)?,
                        Err(e) => write_frame(output, &error_header(&e), &[])?,
                    }
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a request the worker does not know",
                ))
            }
        }
    }
    Ok(())
}

/// A running worker as its parent holds it: where requests go, where frames
/// come from, and how it is stopped (killed and reaped).
pub struct WorkerProcess {
    pub input: Box<dyn Write + Send>,
    pub output: Box<dyn Read + Send>,
    pub stop: Box<dyn FnMut() + Send>,
}

impl WorkerProcess {
    /// A child process started with piped stdin and stdout.
    ///
    /// # Errors
    ///
    /// The child has no piped stdin or stdout.
    pub fn from_child(mut child: std::process::Child) -> io::Result<Self> {
        let missing = || io::Error::other("the worker was started without pipes");
        let input = child.stdin.take().ok_or_else(missing)?;
        let output = child.stdout.take().ok_or_else(missing)?;
        Ok(Self {
            input: Box::new(input),
            output: Box::new(output),
            stop: Box::new(move || {
                let _ = child.kill();
                let _ = child.wait();
            }),
        })
    }
}

/// Starts workers; the caller decides what a worker is and how it is held.
pub trait WorkerSpawner: Send + Sync {
    /// A fresh worker.
    ///
    /// # Errors
    ///
    /// It could not be started (or confined: a worker that cannot be
    /// confined must not be started).
    fn spawn(&self) -> io::Result<WorkerProcess>;
}

/// How many workers a document may use and how long each request may take.
#[derive(Debug, Clone, Copy)]
pub struct WorkerLimits {
    /// Workers at once, each holding its own copy of the document.
    pub workers: usize,
    /// Opening the document (parsing it whole).
    pub open: Duration,
    /// Rendering one page, or the regions of one page.
    pub render: Duration,
    /// Reading the whole text layer, or the outline.
    pub text: Duration,
    /// Every request of one document together: past it, what is left fails
    /// at once, so a document crafted to be slow on every page ends.
    pub total: Duration,
    /// Requests one worker answers before it is replaced, so a long
    /// document never meets the CPU limit a worker sets on itself.
    pub requests_per_worker: usize,
}

impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            workers: 2,
            open: Duration::from_secs(60),
            render: Duration::from_secs(90),
            text: Duration::from_secs(300),
            total: Duration::from_secs(3600),
            requests_per_worker: 200,
        }
    }
}

/// Documents read by workers [`WorkerSpawner`] starts.
pub struct IsolatedBackend {
    spawner: Arc<dyn WorkerSpawner>,
    limits: WorkerLimits,
}

impl IsolatedBackend {
    pub fn new(spawner: Arc<dyn WorkerSpawner>, limits: WorkerLimits) -> Self {
        Self { spawner, limits }
    }
}

impl DocumentBackend for IsolatedBackend {
    fn open(&self, data: Arc<Vec<u8>>) -> Result<Arc<dyn DocumentSource>, DocumentError> {
        if data.len() > MAX_PAYLOAD_BYTES {
            return Err(DocumentError::Invalid(format!(
                "the document is over {MAX_PAYLOAD_BYTES} bytes"
            )));
        }
        let pool = Pool {
            spawner: Arc::clone(&self.spawner),
            limits: self.limits,
            data,
            state: Mutex::new(PoolState {
                idle: Vec::new(),
                live: 1,
                spent: Duration::ZERO,
            }),
            freed: Condvar::new(),
        };
        // The first worker opens the document and says how long it is; its
        // slot is counted from the start.
        let (worker, pages) = pool.start()?;
        pool.give_back(worker);
        Ok(Arc::new(IsolatedSource { pool, pages }))
    }
}

type Frame = io::Result<(Value, Vec<u8>)>;

struct Worker {
    input: Box<dyn Write + Send>,
    frames: Receiver<Frame>,
    stop: Box<dyn FnMut() + Send>,
    answered: usize,
}

impl Worker {
    fn next(
        &self,
        deadline: Instant,
        what: &str,
        limit: Duration,
    ) -> Result<(Value, Vec<u8>), DocumentError> {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.frames.recv_timeout(left) {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(e)) => Err(DocumentError::Invalid(format!(
                "the document worker failed {what}: {e}"
            ))),
            Err(RecvTimeoutError::Timeout) => Err(DocumentError::Invalid(format!(
                "{what} took longer than {} s and was stopped",
                limit.as_secs()
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(DocumentError::Invalid(format!(
                "the document worker ended while {what}"
            ))),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        (self.stop)();
    }
}

struct PoolState {
    idle: Vec<Worker>,
    live: usize,
    /// Time spent in requests so far, against `WorkerLimits::total`.
    spent: Duration,
}

struct Pool {
    spawner: Arc<dyn WorkerSpawner>,
    limits: WorkerLimits,
    data: Arc<Vec<u8>>,
    state: Mutex<PoolState>,
    freed: Condvar,
}

impl Pool {
    /// A new worker with the document open, and its page count.
    fn start(&self) -> Result<(Worker, usize), DocumentError> {
        let process = self.spawner.spawn().map_err(|e| {
            DocumentError::Invalid(format!("the document worker did not start: {e}"))
        })?;
        let (tx, frames) = mpsc::channel();
        let mut output = process.output;
        std::thread::spawn(move || loop {
            let frame = match read_frame(&mut *output) {
                Ok(Some(frame)) => Ok(frame),
                Ok(None) => return,
                Err(e) => Err(e),
            };
            let failed = frame.is_err();
            if tx.send(frame).is_err() || failed {
                return;
            }
        });
        let mut worker = Worker {
            input: process.input,
            frames,
            stop: process.stop,
            answered: 0,
        };
        let deadline = Instant::now() + self.limits.open;
        write_frame(&mut *worker.input, &json!({"op": "open"}), &self.data).map_err(|e| {
            DocumentError::Invalid(format!(
                "the document worker did not take the document: {e}"
            ))
        })?;
        let (header, _) = worker.next(deadline, "opening the document", self.limits.open)?;
        if let Some(e) = error_of(&header) {
            return Err(e);
        }
        let pages = header
            .get("pages")
            .and_then(Value::as_u64)
            .filter(|p| *p > 0)
            .ok_or(DocumentError::Empty)? as usize;
        Ok((worker, pages))
    }

    fn take(&self) -> Result<Worker, DocumentError> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(worker) = state.idle.pop() {
                return Ok(worker);
            }
            if state.live < self.limits.workers.max(1) {
                // The slot is held while the worker starts, so callers at
                // once never start more than the limit.
                state.live += 1;
                drop(state);
                return self.start().map(|(worker, _)| worker).inspect_err(|_| {
                    self.state.lock().unwrap().live -= 1;
                    self.freed.notify_one();
                });
            }
            state = self.freed.wait(state).unwrap();
        }
    }

    fn give_back(&self, worker: Worker) {
        self.state.lock().unwrap().idle.push(worker);
        self.freed.notify_one();
    }

    fn forget(&self, worker: Worker) {
        drop(worker);
        self.state.lock().unwrap().live -= 1;
        self.freed.notify_one();
    }

    /// `frames` answers to one request. A worker that fails or misses the
    /// deadline is stopped and replaced; one that answers is kept.
    fn ask(
        &self,
        request: &Value,
        frames: impl Fn(&Value) -> usize,
        what: &str,
        limit: Duration,
    ) -> Result<Vec<(Value, Vec<u8>)>, DocumentError> {
        let mut worker = self.take()?;
        // Checked once a worker is free: a request that waited for one may
        // have waited while the budget ran out.
        let spent = self.state.lock().unwrap().spent;
        if spent >= self.limits.total {
            self.give_back(worker);
            return Err(DocumentError::Invalid(format!(
                "the document took over {} s to read, so {what} was not tried",
                self.limits.total.as_secs()
            )));
        }
        let started = Instant::now();
        let deadline = started + limit;
        let answered = (|| {
            write_frame(&mut *worker.input, request, &[]).map_err(|e| {
                DocumentError::Invalid(format!("the document worker did not take a request: {e}"))
            })?;
            let first = worker.next(deadline, what, limit)?;
            let more = frames(&first.0);
            let mut all = vec![first];
            for _ in 0..more {
                all.push(worker.next(deadline, what, limit)?);
            }
            Ok(all)
        })();
        self.state.lock().unwrap().spent += started.elapsed();
        match answered {
            Ok(all) => {
                worker.answered += 1;
                if worker.answered >= self.limits.requests_per_worker.max(1) {
                    self.forget(worker);
                } else {
                    self.give_back(worker);
                }
                Ok(all)
            }
            Err(e) => {
                self.forget(worker);
                Err(e)
            }
        }
    }
}

struct IsolatedSource {
    pool: Pool,
    pages: usize,
}

impl IsolatedSource {
    fn list<T: serde::de::DeserializeOwned>(
        &self,
        op: &str,
        what: &str,
    ) -> Result<T, DocumentError> {
        let mut answer = self
            .pool
            .ask(&json!({"op": op}), |_| 0, what, self.pool.limits.text)?;
        let (header, payload) = answer.remove(0);
        if let Some(e) = error_of(&header) {
            return Err(e);
        }
        serde_json::from_slice(&payload).map_err(|e| {
            DocumentError::Invalid(format!(
                "the document worker answered {what} unreadably: {e}"
            ))
        })
    }
}

impl DocumentSource for IsolatedSource {
    fn page_count(&self) -> usize {
        self.pages
    }

    fn native_text(&self) -> Vec<Option<String>> {
        match self.list::<Vec<Option<String>>>("native_text", "reading the text layer") {
            Ok(text) if text.len() == self.pages => text,
            Ok(_) => vec![None; self.pages],
            Err(e) => {
                tracing::warn!(error = %e, "the text layer was not read; every page goes to OCR");
                vec![None; self.pages]
            }
        }
    }

    fn outline(&self) -> Result<Vec<OutlineEntry>, DocumentError> {
        self.list("outline", "reading the outline")
    }

    fn render(&self, index: usize, dpi: f32) -> Result<RenderedPage, DocumentError> {
        let what = format!("rendering page {}", index + 1);
        let mut answer = self.pool.ask(
            &json!({"op": "render", "index": index, "dpi": dpi}),
            |_| 0,
            &what,
            self.pool.limits.render,
        )?;
        let (header, png) = answer.remove(0);
        rendered_of(&header, png)
    }

    fn render_region(
        &self,
        index: usize,
        dpi: f32,
        bbox: [f32; 4],
    ) -> Result<RenderedPage, DocumentError> {
        self.render_regions(index, dpi, &[bbox]).remove(0)
    }

    fn render_regions(
        &self,
        index: usize,
        dpi: f32,
        boxes: &[[f32; 4]],
    ) -> Vec<Result<RenderedPage, DocumentError>> {
        let what = format!("rendering regions of page {}", index + 1);
        let request = json!({"op": "render_regions", "index": index, "dpi": dpi, "boxes": boxes});
        let count = |header: &Value| {
            header
                .get("count")
                .and_then(Value::as_u64)
                .map_or(0, |c| c as usize)
                .min(boxes.len())
        };
        match self
            .pool
            .ask(&request, count, &what, self.pool.limits.render)
        {
            Ok(mut frames) if frames.len() == boxes.len() + 1 => {
                frames.remove(0);
                frames
                    .into_iter()
                    .map(|(header, png)| rendered_of(&header, png))
                    .collect()
            }
            Ok(_) => boxes
                .iter()
                .map(|_| {
                    Err(DocumentError::Invalid(
                        "the document worker answered too few regions".into(),
                    ))
                })
                .collect(),
            Err(e) => boxes
                .iter()
                .map(|_| Err(DocumentError::Invalid(e.to_string())))
                .collect(),
        }
    }
}
