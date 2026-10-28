// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The host's uploader: numbers every session's events, batches them and
//! posts them to the server in order, until the server stores them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sven_control::remote::{EventBatch, EventEnvelope};
use sven_control::{ControlEvent, SessionState};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::MAX_BACKOFF;
use crate::client::{EnvironmentTaken, RemoteClient};

/// The most events one upload carries.
const BATCH_EVENTS: usize = 256;
/// How long the uploader collects before sending what it has.
const BATCH_WINDOW: Duration = Duration::from_millis(150);
/// The longest the host goes without an upload: the server's sign that it is
/// alive (and, with `working`, which answers are still coming). Rare, so
/// thousands of idle hosts cost the server little; anything the host has to
/// say (a command it took, an event) goes at once and counts too.
const HEARTBEAT: Duration = Duration::from_secs(15);

pub(super) enum Upload {
    Event(Box<ControlEvent>),
    /// The highest command `seq` taken.
    Ack(u64),
}

/// Numbers, batches and uploads events in order. A batch is sent again until
/// the server stores it, so nothing is lost while the server is away; the
/// server drops a `(run, seq)` it already has, so nothing is stored twice.
pub(super) async fn upload_loop(client: RemoteClient, mut rx: mpsc::Receiver<Upload>, run: Uuid) {
    // Numbered from 1 within this run; the server tells runs apart by `run`,
    // so a restarted host is never taken for one replaying old events.
    let mut next_seq = 1u64;
    let mut running: HashMap<Uuid, bool> = HashMap::new();
    let mut last_upload = Instant::now();
    loop {
        let mut batch = EventBatch {
            run,
            events: Vec::new(),
            acked_through: None,
            working: Vec::new(),
        };
        let first = tokio::time::timeout(HEARTBEAT, rx.recv()).await;
        let mut take = |upload: Upload, batch: &mut EventBatch| match upload {
            Upload::Ack(seq) => batch.acked_through = Some(seq),
            Upload::Event(event) => {
                let event = *event;
                if let ControlEvent::SessionState { session_id, state } = &event {
                    running.insert(*session_id, *state == SessionState::Running);
                }
                batch.events.push(EventEnvelope {
                    seq: next_seq,
                    event,
                });
                next_seq += 1;
            }
        };
        match first {
            Ok(Some(upload)) => take(upload, &mut batch),
            Ok(None) => return,
            Err(_) => {}
        }
        let window = tokio::time::sleep(BATCH_WINDOW);
        tokio::pin!(window);
        while batch.events.len() < BATCH_EVENTS && !batch.events.is_empty() {
            tokio::select! {
                upload = rx.recv() => match upload {
                    Some(upload) => take(upload, &mut batch),
                    None => break,
                },
                () = &mut window => break,
            }
        }
        // An upload at least every HEARTBEAT, idle or not, is how the server
        // knows this host is alive: a host that froze or lost its network
        // stops sending them, and its environment is let go.
        let heartbeat_due = last_upload.elapsed() >= HEARTBEAT;
        if batch.events.is_empty() && batch.acked_through.is_none() && !heartbeat_due {
            continue;
        }
        batch.working = running
            .iter()
            .filter(|(_, on)| **on)
            .map(|(id, _)| *id)
            .collect();
        let mut backoff = Duration::from_millis(500);
        while let Err(e) = client.post_events(&batch).await {
            // Replaced: nothing more from this run is taken; the command
            // stream is refused too, and that stops the host.
            if e.downcast_ref::<EnvironmentTaken>().is_some() {
                tracing::warn!("event upload: {e:#}");
                return;
            }
            tracing::warn!("event upload: {e:#}");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
        last_upload = Instant::now();
    }
}
