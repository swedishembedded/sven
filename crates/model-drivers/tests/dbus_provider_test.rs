// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Wire-contract test for the D-Bus model transport.
//!
//! There is no real brain server available in CI, so this test *is* the proof
//! that [`sven_model_drivers::dbus::DbusProvider`] speaks brain's protocol correctly.  It
//! stands up a fake `com.swedishembedded.Brain1.Manager` over a plain
//! `UnixStream` pair (peer-to-peer, so no `dbus-daemon` and no well-known name
//! resolution are involved), and the fake reads every received fd the same way
//! the real server does — `fstat` for the length, then `mmap`.
//!
//! That `fstat` is the whole point: a plain anonymous pipe reports
//! `st_size == 0`, so a pipe-backed fd would silently deliver an *empty* blob.
//! The provider must send sealed `memfd`s, and these assertions on exact byte
//! lengths are what catch a regression back to pipes.

#![cfg(all(unix, feature = "dbus"))]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use futures::StreamExt;
use serde_json::Value;
use sven_model::{ContentPart, Message, ModelProvider, ResponseEvent};
use sven_model_drivers::dbus::{blob, DbusOptions, DbusProvider};
use zbus::zvariant::OwnedFd;

// ─── Fixtures ─────────────────────────────────────────────────────────────────

/// A 2×1 PNG: one red pixel, one blue pixel.
///
/// Non-square on purpose, so a width/height mix-up in the meta would show up.
fn test_png() -> Vec<u8> {
    // Built with the `image` crate via sven-image's dependency graph is not
    // available here, so this is a hand-checked 2×1 8-bit RGB PNG.
    // IHDR: 2×1, bit depth 8, colour type 2 (truecolour).
    // IDAT holds one filtered scanline: filter 0, then RGB RGB.
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2u32.to_be_bytes()); // width
    ihdr.extend_from_slice(&1u32.to_be_bytes()); // height
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // depth, colour type, compression, filter, interlace
    png.extend_from_slice(&chunk(b"IHDR", &ihdr));

    // Raw scanline: filter byte 0, then (255,0,0) (0,0,255)
    let raw = [0u8, 255, 0, 0, 0, 0, 255];
    png.extend_from_slice(&chunk(b"IDAT", &zlib_stored(&raw)));
    png.extend_from_slice(&chunk(b"IEND", &[]));
    png
}

/// Wrap `data` in a PNG chunk with the given type and a CRC-32 trailer.
fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

/// A zlib stream using only stored (uncompressed) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // CMF/FLG for deflate, no preset dict
    out.push(0x01); // final block, stored
    out.extend_from_slice(&(data.len() as u16).to_le_bytes());
    out.extend_from_slice(&(!(data.len() as u16)).to_le_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *entry = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// A 16-bit mono WAV: `n` samples at `rate` Hz.
fn test_wav(rate: u32, n: usize) -> Vec<u8> {
    let data = vec![0u8; n * 2];
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

// ─── Fake Brain1.Manager ──────────────────────────────────────────────────────

/// Everything the fake server observed during one `Run` call.
#[derive(Debug, Clone, Default)]
struct Seen {
    model: String,
    action: String,
    params: String,
    in_meta: String,
    transport: String,
    /// Blob name → byte length actually readable through the fd.
    blob_lens: HashMap<String, usize>,
    /// Blob name → the bytes themselves.
    blobs: HashMap<String, Vec<u8>>,
}

/// The canned reply text the fake writes into `out_fds["text"]`.
const FAKE_REPLY: &str = "box: [12, 34, 56, 78]";

struct FakeManager {
    seen: Arc<Mutex<Option<Seen>>>,
}

/// Read an fd exactly the way brain does: `fstat` for the size, then `mmap`.
///
/// A pipe would report `st_size == 0` here and yield an empty vec — which is
/// precisely the silent failure the provider's sealed memfds avoid.
fn read_fd_via_mmap(fd: &OwnedFd) -> Vec<u8> {
    use std::num::NonZeroUsize;
    use std::os::fd::AsRawFd;

    let stat = nix::sys::stat::fstat(fd.as_raw_fd()).expect("fstat on a received fd");
    let len = stat.st_size as usize;
    let Some(nz) = NonZeroUsize::new(len) else {
        return Vec::new();
    };
    unsafe {
        let ptr = nix::sys::mman::mmap(
            None,
            nz,
            nix::sys::mman::ProtFlags::PROT_READ,
            nix::sys::mman::MapFlags::MAP_PRIVATE,
            fd,
            0,
        )
        .expect("mmap of a received fd");
        let slice = std::slice::from_raw_parts(ptr.as_ptr() as *const u8, len);
        let out = slice.to_vec();
        let _ = nix::sys::mman::munmap(ptr, len);
        out
    }
}

#[zbus::interface(name = "com.swedishembedded.Brain1.Manager")]
impl FakeManager {
    #[allow(clippy::too_many_arguments)]
    async fn run(
        &self,
        model: String,
        action: String,
        params: String,
        in_fds: HashMap<String, OwnedFd>,
        in_meta: String,
        transport: String,
    ) -> zbus::fdo::Result<(String, HashMap<String, OwnedFd>, String)> {
        let mut blob_lens = HashMap::new();
        let mut blobs = HashMap::new();
        for (name, fd) in &in_fds {
            let bytes = read_fd_via_mmap(fd);
            blob_lens.insert(name.clone(), bytes.len());
            blobs.insert(name.clone(), bytes);
        }

        *self.seen.lock().unwrap() = Some(Seen {
            model,
            action,
            params,
            in_meta,
            transport,
            blob_lens,
            blobs,
        });

        let text_fd = blob::memfd_with_bytes("text", FAKE_REPLY.as_bytes())
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut out_fds = HashMap::new();
        out_fds.insert("text".to_string(), text_fd);

        Ok((
            r#"{"usage": {"input_tokens": 11, "output_tokens": 5}}"#.to_string(),
            out_fds,
            r#"{"text": {"media": "text"}}"#.to_string(),
        ))
    }

    async fn list_models(&self) -> Vec<String> {
        vec!["brain/omni".to_string(), "brain/qwen-asr".to_string()]
    }

    async fn manifests(&self) -> String {
        r#"{"models": []}"#.to_string()
    }

    #[zbus(property)]
    fn version(&self) -> String {
        "0.0.0-fake".to_string()
    }
}

/// Serve `FakeManager` on one end of a socket pair and return a provider
/// wired to the other end.
async fn fake_server() -> (DbusProvider, Arc<Mutex<Option<Seen>>>, zbus::Connection) {
    let (client_sock, server_sock) = tokio::net::UnixStream::pair().expect("socket pair");
    let seen = Arc::new(Mutex::new(None));

    // Both `build()` calls drive one half of the same authentication
    // handshake, so they must run concurrently — awaiting the server first
    // would deadlock waiting for a client that has not been created yet.
    let seen_for_server = Arc::clone(&seen);
    let server_task = tokio::spawn(async move {
        zbus::connection::Builder::unix_stream(server_sock)
            .p2p()
            .server(zbus::Guid::generate())
            .expect("server guid")
            .serve_at(
                "/com/swedishembedded/Brain1",
                FakeManager {
                    seen: seen_for_server,
                },
            )
            .expect("serve_at")
            .build()
            .await
            .expect("server connection")
    });

    let client_conn = zbus::connection::Builder::unix_stream(client_sock)
        .p2p()
        .build()
        .await
        .expect("client connection");
    let server_conn = server_task.await.expect("server task");

    let provider = DbusProvider::with_connection("brain/omni", DbusOptions::default(), client_conn);
    // The server connection must outlive the call, so hand it back to the test.
    (provider, seen, server_conn)
}

fn png_data_url() -> String {
    format!("data:image/png;base64,{}", B64.encode(test_png()))
}

fn wav_data_url(rate: u32, n: usize) -> String {
    format!("data:audio/wav;base64,{}", B64.encode(test_wav(rate, n)))
}

/// Multimodal request: a system turn, then text + image + audio in one user turn.
fn multimodal_request(audio_rate: u32, audio_samples: usize) -> sven_model::CompletionRequest {
    sven_model::CompletionRequest {
        messages: vec![
            Message::system("You locate objects."),
            Message::user_with_parts(vec![
                ContentPart::text("Follow the spoken instruction."),
                ContentPart::image(png_data_url()),
                ContentPart::audio(wav_data_url(audio_rate, audio_samples)),
            ]),
        ],
        stream: true,
        ..Default::default()
    }
}

/// Drain a response stream into a plain event list.
async fn drain(mut s: sven_model::ResponseStream) -> Vec<ResponseEvent> {
    let mut out = Vec::new();
    while let Some(ev) = s.next().await {
        out.push(ev.expect("stream event"));
    }
    out
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_sends_flattened_messages_as_a_double_encoded_param() {
    let (provider, seen, _server) = fake_server().await;
    let _ = drain(
        provider
            .complete(multimodal_request(16_000, 100))
            .await
            .unwrap(),
    )
    .await;

    let seen = seen.lock().unwrap().clone().expect("Run was called");
    assert_eq!(seen.model, "brain/omni");
    assert_eq!(seen.action, "generate");
    assert_eq!(seen.transport, "memfd");

    let params: Value = serde_json::from_str(&seen.params).expect("params is a JSON object");
    let messages_str = params["messages"]
        .as_str()
        .expect("`messages` must be a JSON *string* value (ParamType::Str)");
    let messages: Value = serde_json::from_str(messages_str).expect("inner messages array");

    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "You locate objects.");
    assert_eq!(messages[1]["role"], "user");
    let user_content = messages[1]["content"].as_str().unwrap();
    assert!(user_content.contains("Follow the spoken instruction."));
    assert!(user_content.contains("[image]"), "{user_content}");
    assert!(user_content.contains("[audio]"), "{user_content}");
    assert!(
        !user_content.contains("base64"),
        "blob payloads must travel on fds, not inside params"
    );
}

#[tokio::test]
async fn image_meta_and_byte_length_match_the_raw_pixel_contract() {
    let (provider, seen, _server) = fake_server().await;
    let _ = drain(
        provider
            .complete(multimodal_request(16_000, 100))
            .await
            .unwrap(),
    )
    .await;

    let seen = seen.lock().unwrap().clone().expect("Run was called");
    let meta: Value = serde_json::from_str(&seen.in_meta).expect("in_meta is JSON");

    // The fixture is a 2×1 RGB PNG.
    assert_eq!(
        meta["image"],
        serde_json::json!({ "media": "image", "w": 2, "h": 1, "c": 3 }),
        "in_meta[\"image\"] must exactly match brain's contract"
    );

    let (w, h) = (2usize, 1usize);
    assert_eq!(
        seen.blob_lens["image"],
        w * h * 3 * 4,
        "image payload must be w*h*3*4 bytes"
    );
}

#[tokio::test]
async fn image_pixels_arrive_as_f32_le_in_unit_range() {
    let (provider, seen, _server) = fake_server().await;
    let _ = drain(
        provider
            .complete(multimodal_request(16_000, 100))
            .await
            .unwrap(),
    )
    .await;

    let seen = seen.lock().unwrap().clone().unwrap();
    let px: Vec<f32> = seen.blobs["image"]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    assert_eq!(px.len(), 6, "2 pixels × 3 channels");
    // Interleaved HWC: first pixel red, second pixel blue.
    assert!((px[0] - 1.0).abs() < 1e-6, "px0 R = {}", px[0]);
    assert!(px[1].abs() < 1e-6, "px0 G = {}", px[1]);
    assert!(px[2].abs() < 1e-6, "px0 B = {}", px[2]);
    assert!(px[3].abs() < 1e-6, "px1 R = {}", px[3]);
    assert!((px[5] - 1.0).abs() < 1e-6, "px1 B = {}", px[5]);
}

#[tokio::test]
async fn audio_meta_and_byte_length_match_the_raw_pcm_contract() {
    let (provider, seen, _server) = fake_server().await;
    let _ = drain(
        provider
            .complete(multimodal_request(16_000, 100))
            .await
            .unwrap(),
    )
    .await;

    let seen = seen.lock().unwrap().clone().unwrap();
    let meta: Value = serde_json::from_str(&seen.in_meta).unwrap();
    assert_eq!(
        meta["audio"],
        serde_json::json!({ "media": "audio", "sample_rate": 16_000 }),
        "in_meta[\"audio\"] must exactly match brain's contract"
    );
    assert_eq!(
        seen.blob_lens["audio"],
        100 * 4,
        "audio payload must be n_samples*4 bytes of f32 PCM"
    );
}

#[tokio::test]
async fn audio_is_resampled_to_16k_before_being_sent() {
    let (provider, seen, _server) = fake_server().await;
    // 200 samples at 32 kHz → 100 samples at 16 kHz.
    let _ = drain(
        provider
            .complete(multimodal_request(32_000, 200))
            .await
            .unwrap(),
    )
    .await;

    let seen = seen.lock().unwrap().clone().unwrap();
    let meta: Value = serde_json::from_str(&seen.in_meta).unwrap();
    assert_eq!(meta["audio"]["sample_rate"], 16_000);
    assert_eq!(seen.blob_lens["audio"], 100 * 4);
}

#[tokio::test]
async fn blobs_are_non_empty_which_a_pipe_backed_fd_would_not_be() {
    let (provider, seen, _server) = fake_server().await;
    let _ = drain(
        provider
            .complete(multimodal_request(16_000, 100))
            .await
            .unwrap(),
    )
    .await;

    let seen = seen.lock().unwrap().clone().unwrap();
    for name in ["image", "audio"] {
        assert!(
            seen.blob_lens[name] > 0,
            "{name} blob read back as empty — the fd must be a sized, mmap-able \
             memfd, not a pipe"
        );
    }
}

#[tokio::test]
async fn stream_yields_the_reply_text_then_usage_then_done() {
    let (provider, _seen, _server) = fake_server().await;
    let events = drain(
        provider
            .complete(multimodal_request(16_000, 100))
            .await
            .unwrap(),
    )
    .await;

    assert!(matches!(events.last(), Some(ResponseEvent::Done)));
    match &events[0] {
        ResponseEvent::TextDelta(t) => assert_eq!(t, FAKE_REPLY),
        other => panic!("expected a TextDelta first, got {other:?}"),
    }
    match &events[1] {
        ResponseEvent::Usage {
            input_tokens,
            output_tokens,
            ..
        } => {
            assert_eq!(*input_tokens, 11);
            assert_eq!(*output_tokens, 5);
        }
        other => panic!("expected Usage second, got {other:?}"),
    }
}

#[tokio::test]
async fn text_only_request_sends_no_blobs() {
    let (provider, seen, _server) = fake_server().await;
    let req = sven_model::CompletionRequest {
        messages: vec![Message::user("just text")],
        ..Default::default()
    };
    let events = drain(provider.complete(req).await.unwrap()).await;
    assert!(matches!(events.last(), Some(ResponseEvent::Done)));

    let seen = seen.lock().unwrap().clone().unwrap();
    assert!(seen.blob_lens.is_empty(), "no fds for a text-only request");
    assert_eq!(seen.in_meta, "{}", "in_meta must be an empty JSON object");
}

#[tokio::test]
async fn only_the_first_image_and_audio_are_sent() {
    let (provider, seen, _server) = fake_server().await;
    let req = sven_model::CompletionRequest {
        messages: vec![
            Message::user_with_parts(vec![
                ContentPart::image(png_data_url()),
                ContentPart::audio(wav_data_url(16_000, 100)),
            ]),
            Message::user_with_parts(vec![
                ContentPart::image(png_data_url()),
                ContentPart::audio(wav_data_url(16_000, 400)),
            ]),
        ],
        ..Default::default()
    };
    let _ = drain(provider.complete(req).await.unwrap()).await;

    let seen = seen.lock().unwrap().clone().unwrap();
    assert_eq!(seen.blob_lens.len(), 2, "exactly one image and one audio");
    assert_eq!(
        seen.blob_lens["audio"],
        100 * 4,
        "the *first* audio blob wins, not the last"
    );
}

#[tokio::test]
async fn list_models_maps_server_ids_to_catalog_entries() {
    let (provider, _seen, _server) = fake_server().await;
    let entries = provider.list_models().await.unwrap();
    let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"brain/omni"), "{ids:?}");
    assert!(ids.contains(&"brain/qwen-asr"), "{ids:?}");
    for e in &entries {
        assert_eq!(e.provider, "dbus");
        assert!(e.supports_images() && e.supports_audio());
    }
}

#[tokio::test]
async fn a_non_data_url_blob_is_rejected_with_a_clear_error() {
    let (provider, _seen, _server) = fake_server().await;
    let req = sven_model::CompletionRequest {
        messages: vec![Message::user_with_parts(vec![ContentPart::image(
            "https://example.com/cat.png",
        )])],
        ..Default::default()
    };
    let msg = match provider.complete(req).await {
        Ok(_) => panic!("a remote image URL must be rejected, not silently dropped"),
        Err(e) => format!("{e:#}"),
    };
    assert!(msg.contains("inline data URL"), "{msg}");
}
