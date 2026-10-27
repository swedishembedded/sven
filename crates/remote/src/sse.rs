// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `data:` payloads of a server-sent event stream, from its bytes as they
//! arrive. Comment lines (keep-alives) and other fields are skipped; one
//! event's several `data:` lines are joined with newlines, as the format says.

/// Bytes in, complete event payloads out.
#[derive(Default)]
pub(crate) struct SseDecoder {
    buffer: String,
}

/// The most one undelivered event may hold. A server that never ends an event
/// would otherwise grow this without bound.
const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

impl SseDecoder {
    /// Feeds `chunk`; returns the payloads of every event it completed.
    ///
    /// # Errors
    ///
    /// The stream is not UTF-8, or one event outgrew [`MAX_EVENT_BYTES`].
    pub(crate) fn push(&mut self, chunk: &[u8]) -> anyhow::Result<Vec<String>> {
        let text = std::str::from_utf8(chunk)
            .map_err(|_| anyhow::anyhow!("the command stream is not UTF-8"))?;
        self.buffer.push_str(&text.replace("\r\n", "\n"));
        let mut events = Vec::new();
        while let Some(end) = self.buffer.find("\n\n") {
            let frame: String = self.buffer.drain(..end + 2).collect();
            let data: Vec<&str> = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|value| value.strip_prefix(' ').unwrap_or(value))
                .collect();
            if !data.is_empty() {
                events.push(data.join("\n"));
            }
        }
        anyhow::ensure!(
            self.buffer.len() <= MAX_EVENT_BYTES,
            "the command stream sent an event larger than {MAX_EVENT_BYTES} bytes"
        );
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_across_chunks_come_out_whole_and_keepalives_do_not() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b": keep-alive\n\n").unwrap().is_empty());
        assert!(decoder.push(b"id: 1\ndata: {\"seq\"").unwrap().is_empty());
        assert_eq!(
            decoder.push(b":1}\r\n\r\ndata: a\ndata: b\n\n").unwrap(),
            vec!["{\"seq\":1}".to_string(), "a\nb".to_string()]
        );
    }
}
