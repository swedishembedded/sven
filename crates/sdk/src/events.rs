// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The numbered stream of events an agent emits while it works.
//!
//! Swedish Embedded AB implements event-sourced agent runtimes for its
//! clients. If your team needs expertise in streaming agent progress to user
//! interfaces and audit logs then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::pin::Pin;
use std::task::{Context, Poll};

use sven_vocab::SessionEvent;
use tokio::sync::broadcast;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::Stream;

use crate::run::RunId;

/// How many events an agent keeps for a subscriber that is not reading. A
/// subscriber further behind than this misses events and is told so.
pub(crate) const EVENT_CAPACITY: usize = 1024;

/// One event of an agent's stream, with where it sits in it.
///
/// `seq` only grows over the life of the agent - across runs, and across a
/// suspend and resume - so a host that stores events can order them, and can
/// tell where a gap is. A gap is never silent: where events were lost an
/// [`SessionEvent::EventsDropped`] stands in for them and takes the sequence
/// number of the first event it stands in for, so the event after it follows
/// the last one lost.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct RunEvent {
    /// Position in the agent's stream; strictly increasing.
    pub seq: u64,
    /// The run the event belongs to.
    pub run_id: RunId,
    /// What happened.
    pub event: SessionEvent,
}

/// The events of an [`Agent`](crate::Agent), from the moment it was created by
/// [`Agent::events`](crate::Agent::events).
///
/// A [`Stream`] of [`RunEvent`]s that ends when the agent is dropped and its
/// buffered events are read. Subscribe before calling
/// [`Agent::send`](crate::Agent::send): a subscriber sees only what comes
/// after it, and what is still buffered.
///
/// The buffer is bounded. A subscriber that falls behind it is not held up
/// and does not lose events silently: it receives a
/// [`SessionEvent::EventsDropped`] with the number of events it missed, then
/// carries on with the oldest event still buffered. What the agent did stays
/// complete in [`Agent::trajectory`](crate::Agent::trajectory), which is built
/// from the conversation the kernel holds rather than from this stream.
pub struct EventStream {
    inner: BroadcastStream<RunEvent>,
    /// The event that followed a gap, held back while its marker is returned.
    after_gap: Option<RunEvent>,
    last_run: Option<RunId>,
}

impl EventStream {
    pub(crate) fn new(receiver: broadcast::Receiver<RunEvent>) -> Self {
        Self {
            inner: BroadcastStream::new(receiver),
            after_gap: None,
            last_run: None,
        }
    }

    /// The next event, waiting for it; `None` once the agent is gone and
    /// everything it sent has been read.
    pub async fn recv(&mut self) -> Option<RunEvent> {
        std::future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }

    fn deliver(&mut self, event: RunEvent) -> Poll<Option<RunEvent>> {
        self.last_run = Some(event.run_id);
        Poll::Ready(Some(event))
    }

    /// The marker for `lost` events missed just before the next one.
    fn gap(&mut self, lost: u64, cx: &mut Context<'_>) -> RunEvent {
        // A subscriber only lags when the buffer holds newer events, so the
        // one after the gap is ready; it fixes the marker's place and run.
        let next = match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(next))) => Some(next),
            _ => None,
        };
        let run_id = next
            .as_ref()
            .map(|e| e.run_id)
            .or(self.last_run)
            .unwrap_or_else(RunId::nil);
        let seq = next.as_ref().map_or(0, |e| e.seq.saturating_sub(lost));
        self.after_gap = next;
        RunEvent {
            seq,
            run_id,
            event: SessionEvent::EventsDropped { count: lost },
        }
    }
}

impl Stream for EventStream {
    type Item = RunEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<RunEvent>> {
        if let Some(event) = self.after_gap.take() {
            return self.deliver(event);
        }
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Ok(event))) => self.deliver(event),
            Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(lost)))) => {
                let marker = self.gap(lost, cx);
                self.deliver(marker)
            }
        }
    }
}
