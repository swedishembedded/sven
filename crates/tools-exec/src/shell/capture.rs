// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent tool output
// bounded and recoverable for its clients. If your team needs expertise in
// running commands for LLM agents without unbounded memory or context growth
// then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Streaming capture of a command's output into an output buffer.
//!
//! Both pipes are read as the bytes arrive, concurrently, so neither can fill
//! and block the command. Whole lines are handed to the buffer so that the
//! two streams interleave on line boundaries; the buffer, not this module,
//! enforces the capture quota, and keeps counting what it no longer retains.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::Mutex;

use sven_tools_fs::OutputBufferStore;

/// Read size per `read` call.
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// A partial line longer than this is handed over as it is, so one endless
/// line cannot grow memory here while waiting for its newline.
const PENDING_LINE_MAX_BYTES: usize = 64 * 1024;

/// How long the pipes get to drain after the shell itself has exited. A
/// background process the command started may hold them open indefinitely.
const PIPE_GRACE: Duration = Duration::from_secs(1);

/// How long the pipes get to drain after a timeout killed the process group.
const KILLED_DRAIN: Duration = Duration::from_secs(1);

/// Where captured bytes go: one buffer of the session's store.
pub(super) struct Sink {
    pub(super) store: Arc<Mutex<OutputBufferStore>>,
    pub(super) handle: String,
}

impl Sink {
    async fn append(&self, bytes: &[u8]) {
        self.store.lock().await.append(&self.handle, bytes);
    }
}

/// Reads one pipe to EOF, passing complete lines to the [`Sink`].
struct Pump<R> {
    reader: R,
    pending: Vec<u8>,
}

impl<R: AsyncRead + Unpin> Pump<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
        }
    }

    /// Read until EOF (or a read error, which ends a pipe the same way).
    /// Cancel safe: unfinished lines stay in `pending` for [`Pump::flush`].
    async fn run(&mut self, sink: &Sink) {
        let mut chunk = vec![0u8; READ_CHUNK_BYTES];
        loop {
            let n = match self.reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = &chunk[..n];
            match chunk.iter().rposition(|&b| b == b'\n') {
                Some(last) => {
                    self.pending.extend_from_slice(&chunk[..=last]);
                    sink.append(&self.pending).await;
                    self.pending.clear();
                    self.pending.extend_from_slice(&chunk[last + 1..]);
                }
                None => self.pending.extend_from_slice(chunk),
            }
            if self.pending.len() >= PENDING_LINE_MAX_BYTES {
                self.flush(sink).await;
            }
        }
        self.flush(sink).await;
    }

    async fn flush(&mut self, sink: &Sink) {
        if !self.pending.is_empty() {
            sink.append(&self.pending).await;
            self.pending.clear();
        }
    }
}

/// How the command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Exit {
    Code(i32),
    /// Ended by a signal it did not handle (Unix).
    Signal(i32),
    /// Still running at the deadline; its process group was killed.
    TimedOut,
    /// Waiting for the command failed.
    WaitFailed(String),
}

impl Exit {
    /// The words after `Exit: ` in a result's footer.
    pub(super) fn describe(&self) -> String {
        match self {
            Exit::Code(code) => code.to_string(),
            Exit::Signal(signal) => format!("killed by signal {signal}"),
            Exit::TimedOut => "timed out, killed".to_string(),
            Exit::WaitFailed(e) => format!("wait failed: {e}"),
        }
    }

    /// Whether the result is an error for the model and the turn's
    /// consecutive-error count.
    ///
    /// Exit 1 without a word of output is the Unix convention for "no
    /// matches" (grep, rg), "condition false" (test, `[`) and the like;
    /// flagging it as an error would inflate the error counter and convince
    /// the model that the command itself broke. Exit 1 with output is a
    /// failure message (a script's traceback, a tool's complaint), so it is
    /// an error like any other non-zero code. The exact code is in the
    /// footer either way.
    pub(super) fn is_failure(&self, silent: bool) -> bool {
        match self {
            Exit::Code(0) => false,
            Exit::Code(1) => !silent,
            _ => true,
        }
    }
}

pub(super) struct Outcome {
    pub(super) exit: Exit,
    pub(super) elapsed: Duration,
}

/// Drive `child` to completion or to `limit`, streaming its output to `sink`.
///
/// On timeout the whole process group is killed and the shell reaped, and
/// what the command wrote before then is kept. Dropping this future (a
/// cancelled call) kills the group too.
pub(super) async fn run_child(mut child: Child, limit: Duration, sink: &Sink) -> Outcome {
    let started = Instant::now();
    #[cfg(unix)]
    let mut group = super::kill_group::KillOnDrop::new(child.id());
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Outcome {
            exit: Exit::WaitFailed("the command's output pipes are missing".into()),
            elapsed: started.elapsed(),
        };
    };
    let mut out = Pump::new(stdout);
    let mut err = Pump::new(stderr);

    let waited = tokio::time::timeout(limit, async {
        let drain = async {
            tokio::join!(out.run(sink), err.run(sink));
        };
        tokio::pin!(drain);
        tokio::select! {
            status = child.wait() => {
                let _ = tokio::time::timeout(PIPE_GRACE, &mut drain).await;
                status
            }
            _ = &mut drain => child.wait().await,
        }
    })
    .await;

    let exit = match waited {
        Ok(Ok(status)) => {
            // It ended by itself: leave anything it started on purpose alone.
            #[cfg(unix)]
            group.disarm();
            exit_of(status)
        }
        Ok(Err(e)) => Exit::WaitFailed(e.to_string()),
        Err(_) => {
            #[cfg(unix)]
            group.kill_now();
            // Reap the shell, then take what was already in the pipes.
            let _ = child.kill().await;
            let _ = tokio::time::timeout(KILLED_DRAIN, async {
                tokio::join!(out.run(sink), err.run(sink));
            })
            .await;
            Exit::TimedOut
        }
    };
    out.flush(sink).await;
    err.flush(sink).await;
    Outcome {
        exit,
        elapsed: started.elapsed(),
    }
}

fn exit_of(status: std::process::ExitStatus) -> Exit {
    if let Some(code) = status.code() {
        return Exit::Code(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Exit::Signal(signal);
        }
    }
    Exit::Code(-1)
}
