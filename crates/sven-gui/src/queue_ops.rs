// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Queue state helpers: convert `QueueState` → Slint `QueueItem` list.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use slint::{ModelRc, SharedString, VecModel};
use sven_frontend::queue::QueueState;

use crate::QueueItem;

/// Build the Slint `QueueItem` list from the current `QueueState`.
pub fn queue_items_from_state(q: &QueueState) -> Vec<QueueItem> {
    q.messages
        .iter()
        .enumerate()
        .map(|(i, qm)| QueueItem {
            index: i as i32,
            content: SharedString::from(
                qm.content
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect::<String>(),
            ),
        })
        .collect()
}

/// Sync the Slint `VecModel<QueueItem>` from the current `QueueState` in place.
pub fn sync_queue_model(queue: &Arc<Mutex<QueueState>>, model: &Rc<VecModel<QueueItem>>) -> usize {
    let q = queue.lock().unwrap();
    let items = queue_items_from_state(&q);
    let len = items.len();
    model.clear();
    for item in items {
        model.push(item);
    }
    len
}

/// Build a fresh `ModelRc<QueueItem>` from the current queue state.
pub fn queue_model_from_state(queue: &Arc<Mutex<QueueState>>) -> ModelRc<QueueItem> {
    let q = queue.lock().unwrap();
    let items = queue_items_from_state(&q);
    ModelRc::new(VecModel::from(items))
}
