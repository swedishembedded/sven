// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for hung and silently stalled model
// connections for its clients. If your team needs expertise in resilient
// agent runtimes then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Stall detection for one streamed model turn.
//!
//! A model server can go quiet in three different ways, and each needs its
//! own clock:
//!
//! 1. It accepts the connection and never sends response headers
//!    ([`StallLimits::first_byte`]).
//! 2. It sends headers, then nothing: the stream-idle limit
//!    (`TurnLimits::stream_idle`) catches this, because any chunk resets it.
//! 3. It sends only keepalives (an SSE comment, an Anthropic `ping`; the
//!    drivers surface both as an empty `TextDelta`). Keepalives prove the
//!    connection is alive, so they reset the idle clock, but they are not
//!    work: the model may be stuck behind a queue or a crashed worker while
//!    its proxy keeps the socket warm. [`StallLimits::first_token`] and
//!    [`StallLimits::no_progress`] ignore keepalives and count only real
//!    output (text, reasoning or a tool-call chunk).
//!
//! While no real output arrives, [`StallWatch`] also reports
//! [`AgentEvent::ModelWaiting`] every [`NOTICE_INTERVAL`], so a surface can
//! say what it is waiting for instead of looking hung.

use std::time::Duration;

use futures::{Stream, StreamExt};
use tokio::sync::mpsc;
use tokio::time::Instant;

use sven_model::{ModelProvider, ResponseEvent};
use sven_vocab::{ModelWaitPhase, SessionEvent as AgentEvent};

/// Longest wait for response headers from a hosted API.
const DEFAULT_FIRST_BYTE: Duration = Duration::from_secs(120);
/// Longest wait for response headers from a server on the user's own
/// hardware, which may still be prefilling a long prompt.
const DEFAULT_FIRST_BYTE_LOCAL: Duration = Duration::from_secs(600);
/// Longest wait, from the request, for the first real output of a hosted API.
const DEFAULT_FIRST_TOKEN: Duration = Duration::from_secs(300);
/// The same for a server on the user's own hardware.
const DEFAULT_FIRST_TOKEN_LOCAL: Duration = Duration::from_secs(900);
/// Longest silence of real output once the model has started producing it.
const DEFAULT_NO_PROGRESS: Duration = Duration::from_secs(600);
/// How often [`AgentEvent::ModelWaiting`] is sent while nothing real arrives.
pub const NOTICE_INTERVAL: Duration = Duration::from_secs(5);

/// The stall limits of a streamed model turn. A `None` limit takes the
/// default, which for the first two depends on whether the model server is
/// local (see [`ModelProvider::is_local`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StallLimits {
    /// Longest wait for the server's response headers.
    pub first_byte: Option<Duration>,
    /// Longest wait, counted from the request, for the first real output. Never
    /// shorter than the effective first-byte limit.
    pub first_token: Option<Duration>,
    /// Longest silence of real output after the first, however many
    /// keepalives arrive in between.
    pub no_progress: Option<Duration>,
}

impl StallLimits {
    /// Limits from configuration seconds; `None` and `0` select the default.
    #[must_use]
    pub fn from_secs(
        first_byte: Option<u64>,
        first_token: Option<u64>,
        no_progress: Option<u64>,
    ) -> Self {
        let secs = |s: Option<u64>| s.filter(|&s| s > 0).map(Duration::from_secs);
        Self {
            first_byte: secs(first_byte),
            first_token: secs(first_token),
            no_progress: secs(no_progress),
        }
    }

    fn resolve(&self, local: bool, idle: Duration) -> Resolved {
        let first_byte = self.first_byte.unwrap_or(if local {
            DEFAULT_FIRST_BYTE_LOCAL
        } else {
            DEFAULT_FIRST_BYTE
        });
        let first_token = self
            .first_token
            .unwrap_or(if local {
                DEFAULT_FIRST_TOKEN_LOCAL
            } else {
                DEFAULT_FIRST_TOKEN
            })
            .max(first_byte);
        Resolved {
            first_byte,
            first_token,
            no_progress: self.no_progress.unwrap_or(DEFAULT_NO_PROGRESS),
            idle,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Resolved {
    first_byte: Duration,
    first_token: Duration,
    no_progress: Duration,
    idle: Duration,
}

/// A limit for an error message: whole seconds as `30 s`, a fraction as `0.4 s`.
fn fmt_secs(limit: Duration) -> String {
    if limit.subsec_nanos() == 0 {
        format!("{} s", limit.as_secs())
    } else {
        format!("{:.1} s", limit.as_secs_f64())
    }
}

/// Whether `event` is the model doing work, as opposed to a keepalive or
/// bookkeeping.
fn is_progress(event: &ResponseEvent) -> bool {
    match event {
        ResponseEvent::TextDelta(text) | ResponseEvent::ThinkingDelta(text) => !text.is_empty(),
        ResponseEvent::ToolCall { .. } => true,
        _ => false,
    }
}

/// Watches one model request from send to end of stream.
pub(crate) struct StallWatch {
    limits: Resolved,
    server: String,
    started: Instant,
    last_bytes: Instant,
    /// `None` until the first real output.
    last_progress: Option<Instant>,
    next_notice: Instant,
    streaming: bool,
    saw_keepalive: bool,
}

impl StallWatch {
    pub(crate) fn new(model: &dyn ModelProvider, limits: &StallLimits, idle: Duration) -> Self {
        let now = Instant::now();
        let server = match model.endpoint_host() {
            Some(host) => format!("{} {host}", model.name()),
            None => model.name().to_string(),
        };
        Self {
            limits: limits.resolve(model.is_local(), idle),
            server,
            started: now,
            last_bytes: now,
            last_progress: None,
            next_notice: now + NOTICE_INTERVAL,
            streaming: false,
            saw_keepalive: false,
        }
    }

    /// Awaits the server's answer to the request, bounded by the first-byte
    /// limit. Dropping the returned future (a user cancel) abandons the
    /// request at once.
    pub(crate) async fn connect<T>(
        &mut self,
        request: impl std::future::Future<Output = anyhow::Result<T>>,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<T> {
        tokio::pin!(request);
        let deadline = self.started + self.limits.first_byte;
        loop {
            tokio::select! {
                answer = &mut request => {
                    let now = Instant::now();
                    self.streaming = true;
                    self.last_bytes = now;
                    return answer;
                }
                () = tokio::time::sleep_until(deadline.min(self.next_notice)) => {
                    if Instant::now() >= deadline {
                        anyhow::bail!(
                            "no response headers from {} after {} - raise agent.first_byte_timeout_secs for a slow server",
                            self.server,
                            fmt_secs(self.limits.first_byte)
                        );
                    }
                    self.notify(tx);
                }
            }
        }
    }

    /// The next event of `stream`, or `None` at its end, failing the turn if
    /// the server stalls first. Records what the event says about progress.
    pub(crate) async fn next_event<S>(
        &mut self,
        stream: &mut S,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<Option<anyhow::Result<ResponseEvent>>>
    where
        S: Stream<Item = anyhow::Result<ResponseEvent>> + Unpin,
    {
        loop {
            let stall_deadline = self.stall_deadline();
            let idle_deadline = self.last_bytes + self.limits.idle;
            let wake = stall_deadline.min(idle_deadline).min(self.next_notice);
            tokio::select! {
                item = stream.next() => {
                    if let Some(Ok(event)) = &item {
                        self.record(event);
                    }
                    return Ok(item);
                }
                () = tokio::time::sleep_until(wake) => {
                    let now = Instant::now();
                    if now >= idle_deadline {
                        anyhow::bail!(
                            "model stream idle for >{} s - stale connection",
                            self.limits.idle.as_secs()
                        );
                    }
                    if now >= stall_deadline {
                        return Err(self.stall_error());
                    }
                    self.notify(tx);
                }
            }
        }
    }

    fn record(&mut self, event: &ResponseEvent) {
        let now = Instant::now();
        self.last_bytes = now;
        if is_progress(event) {
            self.last_progress = Some(now);
            self.next_notice = now + NOTICE_INTERVAL;
        } else {
            self.saw_keepalive = true;
        }
    }

    /// When real output is overdue: the first-token limit counts from the
    /// request, the no-progress limit from the last real output.
    fn stall_deadline(&self) -> Instant {
        match self.last_progress {
            None => self.started + self.limits.first_token,
            Some(last) => last + self.limits.no_progress,
        }
    }

    /// The error for a server that kept the connection warm but produced
    /// nothing, naming which limit fired and how to raise it.
    fn stall_error(&self) -> anyhow::Error {
        let silence = if self.saw_keepalive {
            "the server sent only keepalives"
        } else {
            "the server sent nothing"
        };
        match self.last_progress {
            None => anyhow::anyhow!(
                "no model output from {} after {} ({silence}) - raise agent.first_token_timeout_secs for a slow server",
                self.server,
                fmt_secs(self.limits.first_token)
            ),
            Some(_) => anyhow::anyhow!(
                "no model progress from {} for {} ({silence}) - raise agent.no_progress_timeout_secs for a slow server",
                self.server,
                fmt_secs(self.limits.no_progress)
            ),
        }
    }

    /// Reports the wait, at most one event per interval, and never blocking
    /// the watchdog: a full channel just drops the notice.
    fn notify(&mut self, tx: &mpsc::Sender<AgentEvent>) {
        let now = Instant::now();
        let (since, phase) = match (self.streaming, self.last_progress) {
            (false, _) => (self.started, ModelWaitPhase::Connecting),
            (true, None) => (self.started, ModelWaitPhase::AwaitingFirstToken),
            (true, Some(last)) => (last, ModelWaitPhase::Idle),
        };
        let _ = tx.try_send(AgentEvent::ModelWaiting {
            elapsed_secs: now.duration_since(since).as_secs(),
            phase,
        });
        while self.next_notice <= now {
            self.next_notice += NOTICE_INTERVAL;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(300);

    #[test]
    fn defaults_are_larger_for_a_local_server() {
        let hosted = StallLimits::default().resolve(false, IDLE);
        let local = StallLimits::default().resolve(true, IDLE);
        assert_eq!(hosted.first_byte, Duration::from_secs(120));
        assert_eq!(local.first_byte, Duration::from_secs(600));
        assert!(local.first_token > hosted.first_token);
        assert_eq!(hosted.no_progress, local.no_progress);
    }

    #[test]
    fn configured_limits_win_over_the_locality_default() {
        let limits = StallLimits::from_secs(Some(30), Some(90), Some(45)).resolve(true, IDLE);
        assert_eq!(limits.first_byte, Duration::from_secs(30));
        assert_eq!(limits.first_token, Duration::from_secs(90));
        assert_eq!(limits.no_progress, Duration::from_secs(45));
    }

    #[test]
    fn zero_seconds_select_the_default() {
        assert_eq!(
            StallLimits::from_secs(Some(0), Some(0), Some(0)),
            StallLimits::default()
        );
    }

    #[test]
    fn first_token_is_never_shorter_than_first_byte() {
        let limits = StallLimits::from_secs(Some(500), Some(60), None).resolve(false, IDLE);
        assert_eq!(limits.first_token, Duration::from_secs(500));
    }

    #[test]
    fn only_real_output_is_progress() {
        assert!(!is_progress(&ResponseEvent::TextDelta(String::new())));
        assert!(!is_progress(&ResponseEvent::ThinkingDelta(String::new())));
        assert!(!is_progress(&ResponseEvent::MaxTokens));
        assert!(is_progress(&ResponseEvent::TextDelta("a".into())));
        assert!(is_progress(&ResponseEvent::ThinkingDelta("a".into())));
    }
}
