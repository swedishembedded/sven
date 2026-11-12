// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A person at a terminal as the seat.

use async_trait::async_trait;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

use super::{Provenance, Seat, SeatReply};
use crate::CallError;

/// What the person agreed to; recorded in the transcript's provenance.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveTerms {
    /// The person agreed to the transcript being kept.
    pub consent_recorded: bool,
    /// Days it may be kept; `None` when no limit was agreed.
    pub retention_days: Option<u32>,
}

/// The line that ends a live conversation, besides end of input.
const QUIT: &str = "/done";

/// A seat read from `input`, one line per utterance, with the learner's
/// words shown on `output`.
pub struct LiveSeat<R, W> {
    io: Mutex<(R, W)>,
    terms: LiveTerms,
}

impl<R, W> LiveSeat<R, W>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// A live seat over `input` and `output`.
    pub fn new(input: R, output: W, terms: LiveTerms) -> Self {
        Self {
            io: Mutex::new((input, output)),
            terms,
        }
    }

    async fn read_line(&self, shown: &str) -> Result<SeatReply, CallError> {
        let mut io = self.io.lock().await;
        let (input, output) = &mut *io;
        let infra = |e: std::io::Error| CallError::Infrastructure(e.into());
        if !shown.is_empty() {
            output
                .write_all(format!("{shown}\n").as_bytes())
                .await
                .map_err(infra)?;
        }
        output.write_all(b"you> ").await.map_err(infra)?;
        output.flush().await.map_err(infra)?;
        let mut line = String::new();
        let read = input.read_line(&mut line).await.map_err(infra)?;
        let text = line.trim().to_string();
        Ok(SeatReply {
            done: read == 0 || text == QUIT,
            text: if text == QUIT { String::new() } else { text },
        })
    }
}

#[async_trait]
impl<R, W> Seat for LiveSeat<R, W>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    async fn open(&self) -> Result<SeatReply, CallError> {
        self.read_line("").await
    }

    async fn reply(&self, learner: &str) -> Result<SeatReply, CallError> {
        self.read_line(learner).await
    }

    fn provenance(&self) -> Provenance {
        Provenance::Live {
            consent_recorded: self.terms.consent_recorded,
            retention_days: self.terms.retention_days,
        }
    }
}
