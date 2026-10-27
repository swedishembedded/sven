// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Streaming a completion over brain's `Subscribe`.
//!
//! `Subscribe` returns at once with a job id and the client end of a
//! `SOCK_SEQPACKET` pair; brain then writes one JSON frame per datagram:
//! `progress` (a content `delta` or a structured `event` - reasoning, or a
//! tool call's start and argument pieces), `segment`, `blob` (a memfd sent as
//! `SCM_RIGHTS`), and finally `done` (the result object `Run` returns) or
//! `error`. The response stream therefore starts as soon as the job is
//! admitted, so a long or queued generation never looks like a dead server,
//! and the text arrives as it is generated.
//!
//! Raw `<think>`/tool-call markup never appears in a content delta: brain
//! sends reasoning and tool calls only as structured events, exactly as its
//! HTTP surface renders them into `reasoning_content` and `tool_calls`.
//!
//! Swedish Embedded AB implements low-latency local model transports for
//! agents for its clients. If your team needs expertise in serving models to
//! agent runtimes then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::io::IoSliceMut;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use anyhow::{bail, Context, Result};
use nix::sys::socket::{recvmsg, ControlMessageOwned, MsgFlags};
use serde_json::Value;
use tokio::io::unix::AsyncFd;

use sven_model::ResponseEvent;

use super::reply::{reply_events, reply_extras, reply_usage};

/// Largest frame brain sends: a JSON header (blobs travel as fds).
const MAX_FRAME: usize = 1 << 20;

/// One datagram of the subscription stream.
#[derive(Debug)]
pub(super) enum Frame {
    Progress {
        delta: Option<String>,
        event: Option<Value>,
    },
    Blob {
        name: String,
        fd: Option<OwnedFd>,
    },
    Done {
        result: Value,
    },
    Error {
        message: String,
    },
    /// Frames this client has no use for (`segment`, unknown types).
    Other,
}

impl Frame {
    fn parse(header: &[u8], fd: Option<OwnedFd>) -> Result<Self> {
        let v: Value = serde_json::from_slice(header).context("D-Bus stream frame is not JSON")?;
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        Ok(match v.get("type").and_then(Value::as_str) {
            Some("progress") => Frame::Progress {
                delta: text("delta"),
                event: v.get("event").filter(|e| !e.is_null()).cloned(),
            },
            Some("blob") => Frame::Blob {
                name: text("name").unwrap_or_default(),
                fd,
            },
            Some("done") => Frame::Done {
                result: v.get("result").cloned().unwrap_or(Value::Null),
            },
            Some("error") => Frame::Error {
                message: text("message").unwrap_or_else(|| "unknown error".into()),
            },
            _ => Frame::Other,
        })
    }
}

/// Reads frames off the subscription socket.
pub(super) struct FrameReader {
    fd: AsyncFd<OwnedFd>,
}

impl FrameReader {
    pub(super) fn new(fd: OwnedFd) -> Result<Self> {
        Ok(Self {
            fd: AsyncFd::new(fd).context("watching the D-Bus stream socket")?,
        })
    }

    /// The next frame, or `None` when brain closed the stream.
    pub(super) async fn next(&self) -> Result<Option<Frame>> {
        loop {
            let mut guard = self
                .fd
                .readable()
                .await
                .context("waiting on the D-Bus stream")?;
            match guard.try_io(|inner| receive(inner.get_ref().as_raw_fd())) {
                Ok(Ok(None)) => return Ok(None),
                Ok(Ok(Some((header, fd)))) => return Frame::parse(&header, fd).map(Some),
                Ok(Err(e)) => return Err(e).context("reading the D-Bus stream"),
                Err(_would_block) => continue,
            }
        }
    }
}

/// One datagram and the fd it carries, if any; `None` at end of stream.
fn receive(sock: RawFd) -> std::io::Result<Option<(Vec<u8>, Option<OwnedFd>)>> {
    let mut buf = vec![0u8; MAX_FRAME];
    let mut cmsg = nix::cmsg_space!([RawFd; 1]);
    let mut iov = [IoSliceMut::new(&mut buf)];
    let msg = recvmsg::<()>(sock, &mut iov, Some(&mut cmsg), MsgFlags::MSG_CMSG_CLOEXEC)
        .map_err(std::io::Error::from)?;
    let len = msg.bytes;
    let mut fd = None;
    for c in msg.cmsgs().map_err(std::io::Error::from)? {
        if let ControlMessageOwned::ScmRights(fds) = c {
            // SAFETY: the kernel just installed these descriptors for us.
            fd = fds.first().map(|&raw| unsafe { OwnedFd::from_raw_fd(raw) });
        }
    }
    if len == 0 && fd.is_none() {
        return Ok(None);
    }
    buf.truncate(len);
    Ok(Some((buf, fd)))
}

/// Cancels the brain job when the stream is dropped before its `done` (the
/// session was interrupted or the watchdog gave up), so the server stops
/// generating for nobody.
pub(super) struct CancelOnDrop {
    pub(super) proxy: Option<super::proxy::ManagerProxy<'static>>,
    pub(super) job: u64,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let (Some(proxy), Ok(handle)) =
            (self.proxy.take(), tokio::runtime::Handle::try_current())
        {
            let job = self.job;
            handle.spawn(async move {
                let _ = proxy.cancel(job).await;
            });
        }
    }
}

/// Turns frames into response events, remembering what was already streamed
/// so the final `done` result adds only what the stream did not carry.
#[derive(Default)]
pub(super) struct Translator {
    streamed_text: bool,
    streamed_reasoning: bool,
    streamed_tool_calls: bool,
    text_blob: Option<String>,
}

impl Translator {
    pub(super) fn events(
        &mut self,
        frame: Frame,
        want_logprobs: bool,
    ) -> Result<Vec<ResponseEvent>> {
        Ok(match frame {
            Frame::Progress {
                delta: Some(text), ..
            } if !text.is_empty() => {
                self.streamed_text = true;
                vec![ResponseEvent::TextDelta(text)]
            }
            Frame::Progress {
                event: Some(event), ..
            } => self.event(&event),
            Frame::Progress { .. } | Frame::Other => Vec::new(),
            Frame::Blob { name, fd } => {
                if name == "text" {
                    if let Some(fd) = fd {
                        self.text_blob = Some(super::blob::read_fd_to_string(&fd.into())?);
                    }
                }
                Vec::new()
            }
            Frame::Error { message } => bail!("brain: {message}"),
            Frame::Done { result } => self.finish(&result, want_logprobs),
        })
    }

    fn event(&mut self, event: &Value) -> Vec<ResponseEvent> {
        let text = || {
            event
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
        match event.get("kind").and_then(Value::as_str) {
            Some("reasoning") => {
                self.streamed_reasoning = true;
                vec![ResponseEvent::ThinkingDelta(text())]
            }
            Some("tool_call_start") => {
                self.streamed_tool_calls = true;
                vec![ResponseEvent::ToolCall {
                    index,
                    id: event
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    name: event
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    arguments: String::new(),
                }]
            }
            Some("tool_call_args") => vec![ResponseEvent::ToolCall {
                index,
                id: String::new(),
                name: String::new(),
                arguments: text(),
            }],
            _ => Vec::new(),
        }
    }

    /// The end of the stream: whatever of the result was not streamed, then
    /// the token facts, usage and `Done`.
    fn finish(&mut self, result: &Value, want_logprobs: bool) -> Vec<ResponseEvent> {
        let json = result.to_string();
        let text = self
            .text_blob
            .take()
            .or_else(|| {
                result
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let mut events: Vec<ResponseEvent> = reply_events(&json, text)
            .into_iter()
            .filter(|e| match e {
                ResponseEvent::TextDelta(_) => !self.streamed_text,
                ResponseEvent::ThinkingDelta(_) => !self.streamed_reasoning,
                ResponseEvent::ToolCall { .. } => !self.streamed_tool_calls,
                _ => true,
            })
            .collect();
        events.extend(
            reply_extras(&json)
                .into_iter()
                .filter(|e| want_logprobs || !matches!(e, ResponseEvent::Tokens { .. })),
        );
        let (input_tokens, output_tokens) = reply_usage(&json);
        events.push(ResponseEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: None,
        });
        events.push(ResponseEvent::Done);
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn progress_delta(t: &str) -> Frame {
        Frame::Progress {
            delta: Some(t.into()),
            event: None,
        }
    }
    fn progress_event(e: Value) -> Frame {
        Frame::Progress {
            delta: None,
            event: Some(e),
        }
    }

    #[test]
    fn streamed_text_reasoning_and_tool_calls_are_not_repeated_at_the_end() {
        let mut t = Translator::default();
        let mut all = Vec::new();
        for frame in [
            progress_event(json!({"kind": "reasoning", "text": "need the file"})),
            progress_delta("Reading it."),
            progress_event(
                json!({"kind": "tool_call_start", "index": 0, "id": "c1", "name": "read_file"}),
            ),
            progress_event(
                json!({"kind": "tool_call_args", "index": 0, "text": "{\"path\":\"a\"}"}),
            ),
            Frame::Done {
                result: json!({
                "text": "Reading it.", "reasoning_content": "need the file", "prompt_tokens": 7,
                "completion_tokens": 3, "seed": 9,
                "tool_calls": "[{\"id\":\"c1\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a\\\"}\"}]"}),
            },
        ] {
            all.extend(t.events(frame, false).unwrap());
        }
        let texts = all
            .iter()
            .filter(|e| matches!(e, ResponseEvent::TextDelta(_)))
            .count();
        let thoughts = all
            .iter()
            .filter(|e| matches!(e, ResponseEvent::ThinkingDelta(_)))
            .count();
        let calls: Vec<_> = all
            .iter()
            .filter(|e| matches!(e, ResponseEvent::ToolCall { .. }))
            .collect();
        assert_eq!((texts, thoughts, calls.len()), (1, 1, 2), "{all:?}");
        assert!(matches!(all.last(), Some(ResponseEvent::Done)));
        assert!(all.iter().any(|e| matches!(
            e,
            ResponseEvent::Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..
            }
        )));
        assert!(all
            .iter()
            .any(|e| matches!(e, ResponseEvent::Identity(i) if i.seed == Some(9))));
    }

    #[test]
    fn an_unstreamed_reply_comes_whole_from_the_result() {
        let mut t = Translator::default();
        let events = t
            .events(
                Frame::Done {
                    result: json!({"text": "hello", "prompt_tokens": 1, "completion_tokens": 1}),
                },
                false,
            )
            .unwrap();
        assert!(matches!(&events[0], ResponseEvent::TextDelta(s) if s == "hello"));
    }

    #[test]
    fn an_error_frame_fails_the_stream() {
        let err = Translator::default()
            .events(
                Frame::Error {
                    message: "needs 9 tokens".into(),
                },
                false,
            )
            .unwrap_err();
        assert!(err.to_string().contains("needs 9 tokens"));
    }

    #[tokio::test]
    async fn frames_are_read_off_a_seqpacket_socket_until_it_closes() {
        use nix::sys::socket::{send, socketpair, AddressFamily, SockFlag, SockType};
        let (writer, reader) = socketpair(
            AddressFamily::Unix,
            SockType::SeqPacket,
            None,
            SockFlag::SOCK_NONBLOCK,
        )
        .unwrap();
        for frame in [
            r#"{"type":"progress","delta":"hi"}"#,
            r#"{"type":"done","result":{"text":"hi"}}"#,
        ] {
            send(writer.as_raw_fd(), frame.as_bytes(), MsgFlags::empty()).unwrap();
        }
        drop(writer);
        let reader = FrameReader::new(reader).unwrap();
        assert!(
            matches!(reader.next().await.unwrap(), Some(Frame::Progress { delta: Some(d), .. }) if d == "hi")
        );
        assert!(matches!(
            reader.next().await.unwrap(),
            Some(Frame::Done { .. })
        ));
        assert!(reader.next().await.unwrap().is_none());
    }
}
