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
/// The most image and tool-output payload one upload gathers, so a batch of
/// image-carrying events stays under the server's 24 MB cap on an upload. One
/// event is never split: it goes alone, and the most one carries (four images
/// of 4 MiB, `sven_vocab::ShownImages`) is below the cap.
const BATCH_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
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

/// What an upload counts of `upload` against [`BATCH_PAYLOAD_BYTES`]: the text
/// and images of a finished tool call, the only events that can be large.
fn payload_bytes(upload: &Upload) -> usize {
    match upload {
        Upload::Event(event) => match event.as_ref() {
            ControlEvent::Session {
                event: sven_vocab::SessionEvent::ToolCallFinished { output, images, .. },
                ..
            } => output.len() + images.iter().map(|i| i.data.len()).sum::<usize>(),
            _ => 0,
        },
        Upload::Ack(_) => 0,
    }
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
    // An upload that would have taken a batch over its payload bound, kept
    // to open the next one.
    let mut carried: Option<Upload> = None;
    loop {
        let mut batch = EventBatch {
            run,
            events: Vec::new(),
            acked_through: None,
            working: Vec::new(),
        };
        let first = match carried.take() {
            Some(upload) => Ok(Some(upload)),
            None => tokio::time::timeout(HEARTBEAT, rx.recv()).await,
        };
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
        let mut payload = 0;
        match first {
            Ok(Some(upload)) => {
                payload += payload_bytes(&upload);
                take(upload, &mut batch);
            }
            Ok(None) => return,
            Err(_) => {}
        }
        let window = tokio::time::sleep(BATCH_WINDOW);
        tokio::pin!(window);
        while batch.events.len() < BATCH_EVENTS && !batch.events.is_empty() {
            tokio::select! {
                upload = rx.recv() => match upload {
                    Some(upload) => {
                        let weight = payload_bytes(&upload);
                        if payload + weight > BATCH_PAYLOAD_BYTES {
                            carried = Some(upload);
                            break;
                        }
                        payload += weight;
                        take(upload, &mut batch);
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use sven_vocab::{SessionEvent, ToolImage};

    fn finished(output: &str, image_bytes: usize) -> Upload {
        Upload::Event(Box::new(ControlEvent::Session {
            session_id: Uuid::nil(),
            event: SessionEvent::ToolCallFinished {
                call_id: "c".into(),
                tool_name: "read_file".into(),
                output: output.into(),
                is_error: false,
                images: vec![ToolImage {
                    media_type: "image/png".into(),
                    data: "A".repeat(image_bytes),
                }],
            },
        }))
    }

    #[test]
    fn a_finished_call_weighs_its_text_and_images() {
        assert_eq!(payload_bytes(&finished("abc", 10)), 13);
        assert_eq!(payload_bytes(&Upload::Ack(3)), 0);
    }

    /// The worst result one event carries (four images at their bound) fits
    /// the server's cap alone, and two of them never share an upload.
    const _: () = assert!(BATCH_PAYLOAD_BYTES < 24 * 1024 * 1024);

    #[test]
    fn the_largest_event_fits_an_upload_and_two_of_them_do_not_share_one() {
        let worst = finished(
            "",
            sven_vocab::MAX_IMAGES * sven_vocab::MAX_IMAGE_BASE64_BYTES,
        );
        let weight = payload_bytes(&worst);
        assert!(weight <= BATCH_PAYLOAD_BYTES);
        assert!(2 * weight > BATCH_PAYLOAD_BYTES);
    }
}
