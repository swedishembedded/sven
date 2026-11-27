// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A model request the kernel stops by dropping it (the turn's cancel scope)
//! never reaches the executor's own cancel branch, so nothing would record
//! that the turn was cut off. [`RecordOnDrop`] does it on drop, exactly as that
//! branch does: `Aborted` is observed with the partial reply, which is not
//! filed in the thread (the history holds only replies the model finished);
//! a user-role runtime note `[aborted: <reason>]` says it was stopped.
//! The kernel itself posts the `UserCancelled` that ends the turn.
//!
//! Swedish Embedded AB implements agent runtimes that stop cleanly and say
//! why for its clients. If your team needs expertise in agent cancellation
//! and run bounds, you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::{Arc, Mutex};

use sven_hsm::{ObservationSink, UiEvent};

use crate::thread_store::ThreadStore;
use sven_model::Message;

/// Armed while a model request is in flight; disarmed once the executor has
/// handled its outcome itself.
pub(super) struct RecordOnDrop {
    store: Arc<Mutex<ThreadStore>>,
    thread_id: String,
    partial_text: Arc<Mutex<String>>,
    obs: ObservationSink,
    machine_ends_turn: bool,
    armed: bool,
}

impl RecordOnDrop {
    pub(super) fn new(
        store: Arc<Mutex<ThreadStore>>,
        thread_id: String,
        partial_text: Arc<Mutex<String>>,
        obs: ObservationSink,
        machine_ends_turn: bool,
    ) -> Self {
        Self {
            store,
            thread_id,
            partial_text,
            obs,
            machine_ends_turn,
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
        record_cut(&self.store, &self.thread_id, &reason);
        report_cut(&self.obs, partial, self.machine_ends_turn);
    }
}

/// Files the runtime's note that the reply of `thread` was stopped before it
/// ended, and why. The cut reply itself is not filed: the history holds only
/// what the model finished, and the note is a user-role turn of its own.
pub(super) fn record_cut(store: &Arc<Mutex<ThreadStore>>, thread: &str, reason: &str) {
    if let Ok(mut store) = store.lock() {
        store.append(thread, Message::user(format!("[aborted: {reason}]")));
    }
}

/// Tells observers what a cut-off reply had said. A turn its machine ends
/// is ended by that machine, once, whatever was in flight when it was
/// stopped: the text so far is then only the reply's text (`TextComplete`),
/// never a second ending. Any other turn ends here, with `Aborted`.
pub(super) fn report_cut(obs: &ObservationSink, partial: String, machine_ends_turn: bool) {
    if !machine_ends_turn {
        obs.emit(UiEvent::Aborted {
            partial_text: partial,
        });
    } else if !partial.is_empty() {
        obs.emit(UiEvent::TextComplete(partial));
    }
}
