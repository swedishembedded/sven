// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The manifest while a conversion runs: every settled page is in it, it is
//! rewritten at most every [`CHECKPOINT_EVERY`], and once more when the run
//! ends or is dropped, so a crash or a cancellation loses at most the pages
//! that were in flight.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::DocumentError;
use crate::manifest::{Manifest, PageRecord};

/// Longest time a finished page may exist only in memory.
const CHECKPOINT_EVERY: Duration = Duration::from_secs(2);

pub(crate) struct Checkpoint {
    destination: PathBuf,
    pub(crate) manifest: Manifest,
    last: Instant,
}

impl Checkpoint {
    pub(crate) fn new(destination: &Path, manifest: Manifest) -> Self {
        Self {
            destination: destination.to_path_buf(),
            manifest,
            last: Instant::now(),
        }
    }

    pub(crate) fn push(&mut self, record: PageRecord, checkpoint: bool) {
        self.manifest.pages.retain(|p| p.page != record.page);
        self.manifest.pages.push(record);
        if checkpoint && self.last.elapsed() >= CHECKPOINT_EVERY {
            if let Err(e) = self.flush() {
                tracing::warn!(error = %e, "manifest checkpoint failed");
            }
        }
    }

    pub(crate) fn flush(&mut self) -> Result<(), DocumentError> {
        self.last = Instant::now();
        self.manifest.pages.sort_by_key(|p| p.page);
        self.manifest.write(&self.destination)
    }
}

/// Flushes the manifest when a conversion is dropped before it ends.
pub(crate) struct FlushOnDrop(pub(crate) Arc<Mutex<Checkpoint>>);

impl Drop for FlushOnDrop {
    fn drop(&mut self) {
        if let Ok(mut cp) = self.0.lock() {
            if !cp.manifest.pages.is_empty() {
                let _ = cp.flush();
            }
        }
    }
}
