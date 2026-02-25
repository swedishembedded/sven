//! CBOR codec for the libp2p `request_response` protocol.
//!
//! Wire format per message:
//!   [4 bytes big-endian length][CBOR-encoded payload]
//!
//! Max message size: 8 MiB (covers large multimodal payloads).

use std::io;

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{request_response, StreamProtocol};

use super::types::{P2pRequest, P2pResponse};

const MAX_MSG_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

pub const TASK_PROTO: StreamProtocol =
    StreamProtocol::new("/sven-p2p/task/1.0.0");

// ── Low-level CBOR helpers ────────────────────────────────────────────────────

pub fn cbor_encode<T: serde::Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(buf)
}

pub fn cbor_decode<T: for<'de> serde::Deserialize<'de>>(data: &[u8]) -> io::Result<T> {
    ciborium::from_reader(data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

async fn write_framed<W, T>(io: &mut W, value: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin + Send,
    T: serde::Serialize,
{
    let payload = cbor_encode(value)?;
    if payload.len() > MAX_MSG_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    let len = payload.len() as u32;
    io.write_all(&len.to_be_bytes()).await?;
    io.write_all(&payload).await?;
    io.close().await
}

async fn read_framed<R, T>(io: &mut R) -> io::Result<T>
where
    R: AsyncRead + Unpin + Send,
    T: for<'de> serde::Deserialize<'de>,
{
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_MSG_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "incoming message too large"));
    }
    let mut payload = vec![0u8; len];
    io.read_exact(&mut payload).await?;
    cbor_decode(&payload)
}

// ── Codec implementation ──────────────────────────────────────────────────────

/// libp2p `request_response::Codec` that exchanges CBOR-framed `P2pRequest` /
/// `P2pResponse` messages.
#[derive(Clone, Default, Debug)]
pub struct P2pCodec;

#[async_trait]
impl request_response::Codec for P2pCodec {
    type Protocol = StreamProtocol;
    type Request  = P2pRequest;
    type Response = P2pResponse;

    async fn read_request<T>(&mut self, _proto: &StreamProtocol, io: &mut T) -> io::Result<P2pRequest>
    where T: AsyncRead + Unpin + Send {
        read_framed(io).await
    }

    async fn read_response<T>(&mut self, _proto: &StreamProtocol, io: &mut T) -> io::Result<P2pResponse>
    where T: AsyncRead + Unpin + Send {
        read_framed(io).await
    }

    async fn write_request<T>(&mut self, _proto: &StreamProtocol, io: &mut T, req: P2pRequest) -> io::Result<()>
    where T: AsyncWrite + Unpin + Send {
        write_framed(io, &req).await
    }

    async fn write_response<T>(&mut self, _proto: &StreamProtocol, io: &mut T, resp: P2pResponse) -> io::Result<()>
    where T: AsyncWrite + Unpin + Send {
        write_framed(io, &resp).await
    }
}
