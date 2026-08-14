// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Encoding content parts into brain's raw D-Bus blob wire format.
//!
//! Unlike the HTTP surfaces, the D-Bus transport does **no** server-side
//! decoding: whatever bytes arrive on the fd are handed straight to the model.
//! The client is therefore responsible for producing exactly the layout the
//! model expects:
//!
//! * **image** — interleaved HWC RGB `f32` little-endian in `[0, 1]`,
//!   byte length exactly `w * h * 3 * 4`, described by
//!   `{"media":"image","w":W,"h":H,"c":3}`.
//! * **audio** — mono `f32` little-endian PCM at 16 kHz, byte length
//!   `n_samples * 4`, described by `{"media":"audio","sample_rate":16000}`.
//!
//! The `media` key matters: brain defaults to `bytes` when it is missing or
//! wrong, which makes the model's blob decode fail.

use std::ffi::CString;
use std::io::Write;
use std::os::fd::OwnedFd as StdOwnedFd;

use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use nix::fcntl::{fcntl, FcntlArg, SealFlag};
use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
use serde_json::json;
use zbus::zvariant::OwnedFd;

/// Sample rate every audio blob is resampled to before being sent.
pub const DBUS_AUDIO_RATE: u32 = 16_000;

/// Hard local ceiling on a single blob payload (256 MiB).
///
/// Rejecting oversized payloads here produces a clear error instead of an
/// opaque failure once the fd is already in flight.
pub const MAX_BLOB_BYTES: usize = 256 << 20;

/// A blob ready to be sent: raw payload plus the metadata describing it.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodedBlob {
    pub bytes: Vec<u8>,
    pub meta: serde_json::Value,
}

/// Decode a `data:` URL into its raw bytes.
fn decode_data_url(url: &str, what: &str) -> anyhow::Result<Vec<u8>> {
    let (_mime, b64) = crate::types::parse_data_url_parts(url).map_err(|e| {
        anyhow::anyhow!(
            "the D-Bus transport requires an inline data URL for {what}, \
             but got something else ({e}). Remote URLs are not fetched."
        )
    })?;
    B64.decode(b64.as_bytes())
        .with_context(|| format!("decoding base64 {what} payload"))
}

/// Encode an image data URL as raw HWC `f32` RGB pixels.
///
/// `max_dim` caps the longest side before flattening: raw pixels are ~50×
/// larger than the encoded image, so an uncapped 2048×2048 attachment would
/// produce a 50 MB D-Bus message.
pub fn encode_image(data_url: &str, max_dim: u32) -> anyhow::Result<EncodedBlob> {
    let raw = decode_data_url(data_url, "an image")?;
    let (pixels, w, h) = sven_image::decode_rgb_hwc_f32_max_dim(&raw, max_dim)
        .context("decoding image to raw RGB pixels")?;

    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for p in &pixels {
        bytes.extend_from_slice(&p.to_le_bytes());
    }
    debug_assert_eq!(bytes.len(), (w as usize) * (h as usize) * 3 * 4);
    check_size(bytes.len(), "image")?;

    Ok(EncodedBlob {
        bytes,
        meta: json!({ "media": "image", "w": w, "h": h, "c": 3 }),
    })
}

/// Encode an audio data URL as raw mono `f32` PCM at 16 kHz.
pub fn encode_audio(data_url: &str) -> anyhow::Result<EncodedBlob> {
    let raw = decode_data_url(data_url, "audio")?;
    let pcm = sven_audio::parse_wav(&raw).context("decoding WAV audio")?;
    let samples = sven_audio::resample_linear(&pcm.samples, pcm.sample_rate, DBUS_AUDIO_RATE);
    let bytes = sven_audio::to_f32_le_bytes(&samples);
    check_size(bytes.len(), "audio")?;

    Ok(EncodedBlob {
        bytes,
        meta: json!({ "media": "audio", "sample_rate": DBUS_AUDIO_RATE }),
    })
}

fn check_size(len: usize, what: &str) -> anyhow::Result<()> {
    if len > MAX_BLOB_BYTES {
        bail!(
            "{what} blob is {len} bytes, over the {MAX_BLOB_BYTES} byte limit \
             for a single D-Bus blob; reduce the attachment size"
        );
    }
    Ok(())
}

/// Put `bytes` into a sealed `memfd` and wrap it as a D-Bus file descriptor.
///
/// **A pipe will not do here.** The receiving side sizes the mapping with
/// `fstat` and then `mmap`s it; an anonymous pipe reports `st_size == 0`, so
/// the peer would silently read an *empty* blob with no error anywhere. A
/// memfd (like a regular file) reports its true size and is seekable, so the
/// mmap sees the whole payload.
///
/// The fd is sealed against shrink/grow/write before it is sent so the peer
/// can trust the size it just measured.
pub fn memfd_with_bytes(name: &str, bytes: &[u8]) -> anyhow::Result<OwnedFd> {
    check_size(bytes.len(), name)?;

    let cname = CString::new(format!("sven-{name}")).context("building memfd name")?;
    let fd: StdOwnedFd = memfd_create(
        &cname,
        MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING,
    )
    .context("memfd_create failed")?;

    {
        // Borrow the fd for writing without taking ownership: the File must
        // not close it when it drops.
        let mut file = unsafe {
            use std::os::fd::{AsRawFd, FromRawFd};
            std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(fd.as_raw_fd()))
        };
        file.write_all(bytes).context("writing memfd payload")?;
        file.flush().context("flushing memfd payload")?;
    }

    // Seal so the payload (and its length) cannot change after the peer
    // measures it with fstat.
    {
        use std::os::fd::AsRawFd;
        fcntl(
            fd.as_raw_fd(),
            FcntlArg::F_ADD_SEALS(
                SealFlag::F_SEAL_SHRINK | SealFlag::F_SEAL_GROW | SealFlag::F_SEAL_WRITE,
            ),
        )
        .context("sealing memfd")?;
    }

    Ok(OwnedFd::from(fd))
}

/// Read a received fd to end.
///
/// Reply fds are memfds (regular, seekable files), so a plain read from
/// offset 0 returns the whole payload.
pub fn read_fd_to_string(fd: &OwnedFd) -> anyhow::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::{AsRawFd, FromRawFd};

    let mut file =
        unsafe { std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(fd.as_raw_fd())) };
    // The sender may have left the cursor at the end after writing.
    let _ = file.seek(SeekFrom::Start(0));
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).context("reading reply fd")?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1 red PNG.
    const MINIMAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn png_data_url() -> String {
        format!("data:image/png;base64,{}", B64.encode(MINIMAL_PNG))
    }

    /// 16-bit mono WAV of `n` samples at `rate` Hz.
    fn wav(rate: u32, n: usize) -> Vec<u8> {
        let data = vec![0u8; n * 2];
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    fn wav_data_url(rate: u32, n: usize) -> String {
        format!("data:audio/wav;base64,{}", B64.encode(wav(rate, n)))
    }

    #[test]
    fn image_meta_matches_brain_contract() {
        let blob = encode_image(&png_data_url(), 1024).unwrap();
        assert_eq!(
            blob.meta,
            json!({ "media": "image", "w": 1, "h": 1, "c": 3 })
        );
    }

    #[test]
    fn image_byte_length_is_w_h_3_4() {
        let blob = encode_image(&png_data_url(), 1024).unwrap();
        // 1×1 image, 3 channels, 4 bytes per f32.
        let (w, h) = (1usize, 1usize);
        assert_eq!(blob.bytes.len(), w * h * 3 * 4);
    }

    #[test]
    fn image_pixels_are_f32_le_in_unit_range() {
        let blob = encode_image(&png_data_url(), 1024).unwrap();
        let px: Vec<f32> = blob
            .bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(px.len(), 3);
        assert!((px[0] - 1.0).abs() < 1e-6, "red channel: {}", px[0]);
        assert!(px[1].abs() < 1e-6);
        assert!(px[2].abs() < 1e-6);
    }

    #[test]
    fn image_rejects_non_data_urls() {
        let err = encode_image("https://example.com/a.png", 1024).unwrap_err();
        assert!(err.to_string().contains("inline data URL"), "{err}");
    }

    #[test]
    fn audio_meta_declares_16k_and_media_audio() {
        let blob = encode_audio(&wav_data_url(16_000, 100)).unwrap();
        assert_eq!(
            blob.meta,
            json!({ "media": "audio", "sample_rate": 16_000 })
        );
    }

    #[test]
    fn audio_byte_length_is_samples_times_four() {
        let blob = encode_audio(&wav_data_url(16_000, 100)).unwrap();
        assert_eq!(blob.bytes.len(), 100 * 4);
    }

    #[test]
    fn audio_is_resampled_to_16k() {
        // 32 kHz input halves in length on the way to 16 kHz.
        let blob = encode_audio(&wav_data_url(32_000, 200)).unwrap();
        assert_eq!(blob.bytes.len(), 100 * 4);
    }

    #[test]
    fn audio_rejects_non_wav_payloads() {
        let url = format!("data:audio/wav;base64,{}", B64.encode(b"not a wav"));
        assert!(encode_audio(&url).is_err());
    }

    #[test]
    fn memfd_round_trips_its_payload() {
        let payload = b"hello brain".to_vec();
        let fd = memfd_with_bytes("test", &payload).unwrap();
        let text = read_fd_to_string(&fd).unwrap();
        assert_eq!(text, "hello brain");
    }

    #[test]
    fn memfd_reports_its_real_size_via_fstat() {
        // This is the property a pipe would violate (st_size == 0), silently
        // yielding an empty blob on the receiving side.
        use std::os::fd::AsRawFd;
        let payload = vec![7u8; 4096];
        let fd = memfd_with_bytes("sized", &payload).unwrap();
        let stat = nix::sys::stat::fstat(fd.as_raw_fd()).unwrap();
        assert_eq!(stat.st_size as usize, payload.len());
    }

    #[test]
    fn memfd_is_sealed_against_writes() {
        use std::io::Write as _;
        use std::os::fd::{AsRawFd, FromRawFd};
        let fd = memfd_with_bytes("sealed", b"abc").unwrap();
        let mut file =
            unsafe { std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(fd.as_raw_fd())) };
        assert!(
            file.write_all(b"tamper").is_err(),
            "a sealed memfd must reject further writes"
        );
    }

    #[test]
    fn memfd_handles_an_empty_payload() {
        let fd = memfd_with_bytes("empty", &[]).unwrap();
        assert_eq!(read_fd_to_string(&fd).unwrap(), "");
    }

    #[test]
    fn oversized_payload_is_rejected_before_creating_an_fd() {
        let err = check_size(MAX_BLOB_BYTES + 1, "image").unwrap_err();
        assert!(err.to_string().contains("over the"), "{err}");
    }
}
