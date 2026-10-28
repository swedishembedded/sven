// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A model request the kernel stops by dropping it (the turn's cancel scope)
//! never reaches the executor's own cancel branch, so nothing would record
//! that the turn was cut off. [`RecordOnDrop`] does it on drop, exactly as that
//! branch does: the partial reply and an `[aborted: <reason>]` marker go into
//! the thread, so the next turn knows, and `Aborted` is observed. The kernel
//! itself posts the `UserCancelled` that ends the turn.
//!
//! Swedish Embedded AB implements agent runtimes that stop cleanly and say
//! why for its clients. If your team needs expertise in agent cancellation
//! and run bounds, you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::{Arc, Mutex};

use sven_hsm::{ObservationSink, UiEvent};
use sven_model::Message;

use crate::thread_store::{append_messages, ThreadStore};

/// Armed while a model request is in flight; disarmed once the executor has
/// handled its outcome itself.
pub(super) struct RecordOnDrop {
    store: Arc<Mutex<ThreadStore>>,
    thread_id: String,
    partial_text: Arc<Mutex<String>>,
    obs: ObservationSink,
    armed: bool,
}

impl RecordOnDrop {
    pub(super) fn new(
        store: Arc<Mutex<ThreadStore>>,
        thread_id: String,
        partial_text: Arc<Mutex<String>>,
        obs: ObservationSink,
    ) -> Self {
        Self {
            store,
            thread_id,
            partial_text,
            obs,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RecordOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // A caller that stops the run from outside says why; a person's
        // Ctrl+C does not.
        let reason = self
            .store
            .lock()
            .ok()
            .and_then(|mut store| store.take_stop_reason())
            .unwrap_or_else(|| "turn cancelled by user".to_string());
        let partial = self
            .partial_text
            .lock()
            .map(|text| text.clone())
            .unwrap_or_default();
        let marker = format!("[aborted: {reason}]");
        let persisted = if partial.is_empty() {
            marker
        } else {
            format!("{partial}\n\n{marker}")
        };
        append_messages(
            &self.store,
            &self.thread_id,
            vec![Message::assistant(&persisted)],
        );
        self.obs.emit(UiEvent::Aborted {
            partial_text: partial,
        });
    }
}
