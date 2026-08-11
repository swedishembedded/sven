// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Audio loading, WAV decoding, and resampling utilities for sven.
//!
//! This crate turns local audio files into the two shapes sven's model
//! transports need:
//!
//! * [`load_audio_data_url`] — the *undecoded* file bytes as a
//!   `data:audio/wav;base64,…` URL, for the HTTP transports (OpenAI-compatible
//!   `input_audio`, Gemini `inline_data`) which decode server-side.
//! * [`load_pcm_at`] — real decoded mono `f32` samples at a chosen rate, for
//!   the D-Bus transport (which does no server-side decoding at all) and for
//!   the ASR fallback.
//!
//! ## Format support
//! Only uncompressed RIFF/WAVE is decoded: 8/16/24/32-bit integer PCM and
//! 32-bit IEEE float, any channel count (downmixed by averaging).  Compressed
//! formats (MP3, FLAC, Ogg, M4A) are *recognised* by [`is_audio_extension`]
//! but produce a specific [`AudioError::UnsupportedFormat`] rather than
//! silently misbehaving.

use std::path::Path;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

pub use error::AudioError;

mod error;

/// Largest audio file accepted, in bytes (25 MiB).
///
/// Matches the upload cap of the major provider APIs; anything larger is far
/// past the point where an inline base64 attachment is reasonable.
pub const MAX_AUDIO_BYTES: usize = 25 << 20;

/// Decoded mono audio: `samples` are normalised to `[-1.0, 1.0]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

impl Pcm {
    /// Duration in seconds (0.0 when the sample rate is unknown).
    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.samples.len() as f32 / self.sample_rate as f32
    }
}

/// Summary of an audio file's properties without keeping the samples around.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioSpec {
    /// Sample rate as stored in the file (before any resampling).
    pub sample_rate: u32,
    /// Channel count as stored in the file (before downmixing).
    pub channels: u16,
    /// Duration in seconds.
    pub duration_secs: f32,
}

// ─── WAV parsing ──────────────────────────────────────────────────────────────

/// WAVE format tags we understand (from the `fmt ` chunk).
const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

fn u16_le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Parse a RIFF/WAVE byte buffer into mono `f32` samples.
///
/// Supports 8/16/24/32-bit integer PCM and 32-bit IEEE float, with any channel
/// count — multi-channel input is downmixed by averaging the channels.
///
/// `WAVE_FORMAT_EXTENSIBLE` is resolved through the first two bytes of its
/// SubFormat GUID, which carry the real format tag.
pub fn parse_wav(bytes: &[u8]) -> Result<Pcm, AudioError> {
    if bytes.len() < 12 {
        return Err(AudioError::NotWav(format!(
            "file is only {} bytes; a RIFF header needs at least 12",
            bytes.len()
        )));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(AudioError::NotWav(
            "missing 'RIFF'/'WAVE' magic in the first 12 bytes".to_string(),
        ));
    }

    let mut fmt: Option<WavFmt> = None;
    let mut data: Option<&[u8]> = None;

    // Walk the chunk list.  Chunk bodies are padded to an even length.
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_le(bytes, pos + 4) as usize;
        let body_start = pos + 8;
        // Tolerate a truncated final chunk (common with streamed WAVs whose
        // header size field was never patched) by clamping to the buffer.
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];

        match id {
            b"fmt " => fmt = Some(parse_fmt_chunk(body)?),
            b"data" => data = Some(body),
            _ => {}
        }

        pos = body_end + (size % 2);
        if body_end == bytes.len() {
            break;
        }
    }

    let fmt = fmt.ok_or_else(|| AudioError::Malformed("no 'fmt ' chunk found".to_string()))?;
    let data = data.ok_or_else(|| AudioError::Malformed("no 'data' chunk found".to_string()))?;

    if fmt.channels == 0 {
        return Err(AudioError::Malformed("channel count is 0".to_string()));
    }
    if fmt.sample_rate == 0 {
        return Err(AudioError::Malformed("sample rate is 0".to_string()));
    }

    let interleaved = decode_samples(data, &fmt)?;
    let samples = downmix(interleaved, fmt.channels);

    Ok(Pcm {
        sample_rate: fmt.sample_rate,
        samples,
    })
}

/// Parsed contents of a WAV `fmt ` chunk.
struct WavFmt {
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
}

fn parse_fmt_chunk(body: &[u8]) -> Result<WavFmt, AudioError> {
    if body.len() < 16 {
        return Err(AudioError::Malformed(format!(
            "'fmt ' chunk is {} bytes; at least 16 are required",
            body.len()
        )));
    }
    let mut format_tag = u16_le(body, 0);
    let channels = u16_le(body, 2);
    let sample_rate = u32_le(body, 4);
    let bits_per_sample = u16_le(body, 14);

    // WAVE_FORMAT_EXTENSIBLE stores the real tag in the first two bytes of the
    // 16-byte SubFormat GUID, which starts at offset 24 of the chunk body.
    if format_tag == WAVE_FORMAT_EXTENSIBLE {
        if body.len() < 26 {
            return Err(AudioError::Malformed(
                "WAVE_FORMAT_EXTENSIBLE 'fmt ' chunk is missing its SubFormat GUID".to_string(),
            ));
        }
        format_tag = u16_le(body, 24);
    }

    Ok(WavFmt {
        format_tag,
        channels,
        sample_rate,
        bits_per_sample,
    })
}

/// Convert the raw `data` chunk into interleaved `f32` samples in `[-1, 1]`.
fn decode_samples(data: &[u8], fmt: &WavFmt) -> Result<Vec<f32>, AudioError> {
    match (fmt.format_tag, fmt.bits_per_sample) {
        (WAVE_FORMAT_PCM, 8) => {
            // 8-bit WAV PCM is *unsigned* with a 128 midpoint.
            Ok(data
                .iter()
                .map(|&b| (b as f32 - 128.0) / 128.0)
                .collect::<Vec<_>>())
        }
        (WAVE_FORMAT_PCM, 16) => Ok(data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32_768.0)
            .collect()),
        (WAVE_FORMAT_PCM, 24) => Ok(data
            .chunks_exact(3)
            .map(|c| {
                // Sign-extend 24-bit little-endian into i32.
                let v = ((c[2] as i32) << 24 | (c[1] as i32) << 16 | (c[0] as i32) << 8) >> 8;
                v as f32 / 8_388_608.0
            })
            .collect()),
        (WAVE_FORMAT_PCM, 32) => Ok(data
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0)
            .collect()),
        (WAVE_FORMAT_IEEE_FLOAT, 32) => Ok(data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()),
        (WAVE_FORMAT_IEEE_FLOAT, 64) => Ok(data
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]) as f32)
            .collect()),
        (tag, bits) => Err(AudioError::UnsupportedSampleFormat(format!(
            "format tag 0x{tag:04X} with {bits} bits per sample"
        ))),
    }
}

/// Average interleaved channels down to a single mono track.
fn downmix(interleaved: Vec<f32>, channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return interleaved;
    }
    let ch = channels as usize;
    let inv = 1.0 / ch as f32;
    interleaved
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() * inv)
        .collect()
}

// ─── Resampling ───────────────────────────────────────────────────────────────

/// Resample `samples` from `src_rate` to `dst_rate` by linear interpolation.
///
/// Good enough for speech fed to an ASR or omni model; no anti-aliasing filter
/// is applied, so heavy downsampling of wideband music will alias.
pub fn resample_linear(samples: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if samples.is_empty() || src_rate == 0 || dst_rate == 0 || src_rate == dst_rate {
        return samples.to_vec();
    }
    let ratio = dst_rate as f64 / src_rate as f64;
    let out_len = ((samples.len() as f64) * ratio).round() as usize;
    if out_len == 0 {
        return Vec::new();
    }
    let step = 1.0 / ratio;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * step;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = samples[idx.min(samples.len() - 1)];
        let b = samples[(idx + 1).min(samples.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

/// Serialise samples as little-endian `f32` bytes.
///
/// This is the raw-PCM wire convention used by the D-Bus transport and by
/// `brain do`'s blob loader, which read the payload with no format sniffing.
pub fn to_f32_le_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

// ─── File loading ─────────────────────────────────────────────────────────────

/// Read an audio file, enforcing [`MAX_AUDIO_BYTES`].
fn read_capped(path: &Path) -> Result<Vec<u8>, AudioError> {
    let bytes = std::fs::read(path).map_err(|e| AudioError::Io(path.display().to_string(), e))?;
    if bytes.len() > MAX_AUDIO_BYTES {
        return Err(AudioError::TooLarge {
            path: path.display().to_string(),
            size: bytes.len(),
            limit: MAX_AUDIO_BYTES,
        });
    }
    Ok(bytes)
}

/// Reject files whose extension names a format we have no decoder for.
///
/// Returns a specific "no decoder" error so callers can say something more
/// useful than "malformed WAV" when handed an MP3.
fn reject_undecodable_extension(path: &Path) -> Result<(), AudioError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp3" | "m4a" | "flac" | "ogg" | "oga" | "opus" | "aac" | "wma" => {
            Err(AudioError::UnsupportedFormat { format: ext })
        }
        _ => Ok(()),
    }
}

/// Load `path` and return it as `data:audio/wav;base64,<b64>`.
///
/// The file bytes are embedded **verbatim** — not decoded, resampled, or
/// re-encoded — because the HTTP transports decode the container server-side
/// and re-encoding would only lose fidelity.
pub fn load_audio_data_url(path: &Path) -> Result<String, AudioError> {
    reject_undecodable_extension(path)?;
    let bytes = read_capped(path)?;
    // Validate that this really is a WAV before handing it to a server that
    // only accepts WAV; a clear local error beats a remote 400.
    parse_wav(&bytes)?;
    Ok(format!("data:audio/wav;base64,{}", B64.encode(&bytes)))
}

/// Load `path` as mono `f32` PCM resampled to exactly `target_rate`.
pub fn load_pcm_at(path: &Path, target_rate: u32) -> Result<Pcm, AudioError> {
    reject_undecodable_extension(path)?;
    let bytes = read_capped(path)?;
    let pcm = parse_wav(&bytes)?;
    Ok(resample_pcm(pcm, target_rate))
}

/// Resample an already-decoded [`Pcm`] to `target_rate`.
pub fn resample_pcm(pcm: Pcm, target_rate: u32) -> Pcm {
    if pcm.sample_rate == target_rate || target_rate == 0 {
        return pcm;
    }
    let samples = resample_linear(&pcm.samples, pcm.sample_rate, target_rate);
    Pcm {
        sample_rate: target_rate,
        samples,
    }
}

/// Read only the properties of an audio file (rate, channels, duration).
///
/// Used for the human-readable "Attached audio: … (3.2s, 16000 Hz)" line
/// without having to keep the decoded samples alive.
pub fn probe(path: &Path) -> Result<AudioSpec, AudioError> {
    reject_undecodable_extension(path)?;
    let bytes = read_capped(path)?;
    probe_bytes(&bytes)
}

/// [`probe`] for an in-memory buffer.
pub fn probe_bytes(bytes: &[u8]) -> Result<AudioSpec, AudioError> {
    // Channel count lives in the fmt chunk; parse_wav has already downmixed,
    // so read the header separately to report the original channel count.
    let channels = wav_channels(bytes).unwrap_or(1);
    let pcm = parse_wav(bytes)?;
    Ok(AudioSpec {
        sample_rate: pcm.sample_rate,
        channels,
        duration_secs: pcm.duration_secs(),
    })
}

/// Read just the channel count from a WAV `fmt ` chunk, if present.
fn wav_channels(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_le(bytes, pos + 4) as usize;
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        if id == b"fmt " && body_end - body_start >= 4 {
            return Some(u16_le(bytes, body_start + 2));
        }
        pos = body_end + (size % 2);
        if body_end == bytes.len() {
            break;
        }
    }
    None
}

/// Decode a `data:audio/…;base64,<b64>` URL into `(mime, raw_bytes)`.
pub fn parse_data_url(data_url: &str) -> Result<(String, Vec<u8>), AudioError> {
    let rest = data_url
        .strip_prefix("data:")
        .ok_or_else(|| AudioError::InvalidDataUrl(truncate_for_error(data_url)))?;
    let (meta, b64) = rest
        .split_once(',')
        .ok_or_else(|| AudioError::InvalidDataUrl(truncate_for_error(data_url)))?;
    let mime = meta.strip_suffix(";base64").unwrap_or(meta).to_string();
    let bytes = B64
        .decode(b64)
        .map_err(|e| AudioError::Base64(e.to_string()))?;
    Ok((mime, bytes))
}

/// Keep error messages readable when the offending value is a huge data URL.
fn truncate_for_error(s: &str) -> String {
    if s.len() <= 64 {
        s.to_string()
    } else {
        format!("{}…", &s[..64])
    }
}

/// Return whether `ext` names an audio format sven is willing to accept.
///
/// Deliberately broader than what [`parse_wav`] can decode: the extra formats
/// are accepted by the attachment front-end so the user gets a specific
/// "no decoder" message instead of "not an audio file".
pub fn is_audio_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "wav" | "wave" | "mp3" | "m4a" | "flac" | "ogg"
    )
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a RIFF/WAVE file by hand.
    ///
    /// `data` is the raw sample payload; the header is a canonical 44-byte
    /// PCM header (RIFF / fmt / data, no extra chunks).
    fn build_wav(
        format_tag: u16,
        channels: u16,
        sample_rate: u32,
        bits: u16,
        data: &[u8],
    ) -> Vec<u8> {
        let block_align = channels * bits / 8;
        let byte_rate = sample_rate * block_align as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&format_tag.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    /// 16-bit mono WAV from i16 sample values.
    fn wav_i16_mono(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        let mut data = Vec::new();
        for s in samples {
            data.extend_from_slice(&s.to_le_bytes());
        }
        build_wav(WAVE_FORMAT_PCM, 1, sample_rate, 16, &data)
    }

    // ── parse_wav ─────────────────────────────────────────────────────────────

    #[test]
    fn parses_16bit_mono_and_round_trips_values() {
        let raw: [i16; 5] = [0, 16_384, -16_384, 32_767, -32_768];
        let bytes = wav_i16_mono(16_000, &raw);
        let pcm = parse_wav(&bytes).expect("should parse");
        assert_eq!(pcm.sample_rate, 16_000);
        assert_eq!(pcm.samples.len(), 5);
        assert!((pcm.samples[0] - 0.0).abs() < 1e-6);
        assert!((pcm.samples[1] - 0.5).abs() < 1e-6);
        assert!((pcm.samples[2] + 0.5).abs() < 1e-6);
        assert!((pcm.samples[3] - 0.999_969).abs() < 1e-4);
        assert!((pcm.samples[4] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn header_is_44_bytes_for_canonical_pcm() {
        // Sanity check on the hand-built header used by every other test.
        let bytes = wav_i16_mono(8_000, &[0]);
        assert_eq!(bytes.len(), 44 + 2);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(&bytes[36..40], b"data");
    }

    #[test]
    fn duration_secs_matches_sample_count() {
        let bytes = wav_i16_mono(8_000, &vec![0i16; 4_000]);
        let pcm = parse_wav(&bytes).unwrap();
        assert!((pcm.duration_secs() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn downmixes_stereo_by_averaging() {
        // Interleaved L/R: (1.0, -1.0) → 0.0 ; (0.5, 0.5) → 0.5
        let raw: [i16; 4] = [32_767, -32_768, 16_384, 16_384];
        let mut data = Vec::new();
        for s in raw {
            data.extend_from_slice(&s.to_le_bytes());
        }
        let bytes = build_wav(WAVE_FORMAT_PCM, 2, 44_100, 16, &data);
        let pcm = parse_wav(&bytes).unwrap();
        assert_eq!(pcm.samples.len(), 2, "stereo frames collapse to mono");
        assert!(pcm.samples[0].abs() < 1e-3, "got {}", pcm.samples[0]);
        assert!(
            (pcm.samples[1] - 0.5).abs() < 1e-4,
            "got {}",
            pcm.samples[1]
        );
    }

    #[test]
    fn downmixes_four_channels() {
        // One frame of four channels: 1.0, 0.0, 0.0, 0.0 → mean 0.25
        let raw: [i16; 4] = [32_767, 0, 0, 0];
        let mut data = Vec::new();
        for s in raw {
            data.extend_from_slice(&s.to_le_bytes());
        }
        let bytes = build_wav(WAVE_FORMAT_PCM, 4, 16_000, 16, &data);
        let pcm = parse_wav(&bytes).unwrap();
        assert_eq!(pcm.samples.len(), 1);
        assert!((pcm.samples[0] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn parses_8bit_unsigned_pcm() {
        // 0 → -1.0, 128 → 0.0, 255 → ~+0.99
        let data = [0u8, 128, 255];
        let bytes = build_wav(WAVE_FORMAT_PCM, 1, 8_000, 8, &data);
        let pcm = parse_wav(&bytes).unwrap();
        assert!((pcm.samples[0] + 1.0).abs() < 1e-6);
        assert!(pcm.samples[1].abs() < 1e-6);
        assert!((pcm.samples[2] - 0.992).abs() < 1e-2);
    }

    #[test]
    fn parses_24bit_pcm_including_negatives() {
        // 0x000000 → 0.0 ; 0x400000 → +0.5 ; 0xC00000 → -0.5
        let data = [0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0xC0];
        let bytes = build_wav(WAVE_FORMAT_PCM, 1, 16_000, 24, &data);
        let pcm = parse_wav(&bytes).unwrap();
        assert_eq!(pcm.samples.len(), 3);
        assert!(pcm.samples[0].abs() < 1e-6);
        assert!((pcm.samples[1] - 0.5).abs() < 1e-6);
        assert!((pcm.samples[2] + 0.5).abs() < 1e-6);
    }

    #[test]
    fn parses_32bit_int_pcm() {
        let vals: [i32; 2] = [0, 1_073_741_824]; // 0.0, +0.5
        let mut data = Vec::new();
        for v in vals {
            data.extend_from_slice(&v.to_le_bytes());
        }
        let bytes = build_wav(WAVE_FORMAT_PCM, 1, 16_000, 32, &data);
        let pcm = parse_wav(&bytes).unwrap();
        assert!(pcm.samples[0].abs() < 1e-6);
        assert!((pcm.samples[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn parses_32bit_float_pcm() {
        let vals: [f32; 3] = [0.0, 0.25, -0.75];
        let mut data = Vec::new();
        for v in vals {
            data.extend_from_slice(&v.to_le_bytes());
        }
        let bytes = build_wav(WAVE_FORMAT_IEEE_FLOAT, 1, 48_000, 32, &data);
        let pcm = parse_wav(&bytes).unwrap();
        assert_eq!(pcm.sample_rate, 48_000);
        assert_eq!(pcm.samples, vals.to_vec());
    }

    #[test]
    fn skips_unknown_chunks_before_data() {
        // Insert a LIST chunk between fmt and data.
        let mut bytes = wav_i16_mono(16_000, &[16_384]);
        let list: Vec<u8> = {
            let payload = b"INFOhello!!!"; // 12 bytes (even)
            let mut c = Vec::new();
            c.extend_from_slice(b"LIST");
            c.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            c.extend_from_slice(payload);
            c
        };
        // Splice the LIST chunk in at offset 36 (right before "data").
        let tail = bytes.split_off(36);
        bytes.extend_from_slice(&list);
        bytes.extend_from_slice(&tail);
        let pcm = parse_wav(&bytes).expect("unknown chunks must be skipped");
        assert_eq!(pcm.samples.len(), 1);
        assert!((pcm.samples[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn parses_wave_format_extensible_as_pcm() {
        // Build a 40-byte fmt chunk with WAVE_FORMAT_EXTENSIBLE + PCM SubFormat.
        let data: Vec<u8> = 16_384i16.to_le_bytes().to_vec();
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&WAVE_FORMAT_EXTENSIBLE.to_le_bytes());
        fmt.extend_from_slice(&1u16.to_le_bytes()); // channels
        fmt.extend_from_slice(&16_000u32.to_le_bytes()); // sample rate
        fmt.extend_from_slice(&32_000u32.to_le_bytes()); // byte rate
        fmt.extend_from_slice(&2u16.to_le_bytes()); // block align
        fmt.extend_from_slice(&16u16.to_le_bytes()); // bits
        fmt.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        fmt.extend_from_slice(&16u16.to_le_bytes()); // valid bits
        fmt.extend_from_slice(&0u32.to_le_bytes()); // channel mask
        fmt.extend_from_slice(&WAVE_FORMAT_PCM.to_le_bytes()); // SubFormat GUID [0..2]
        fmt.extend_from_slice(&[0u8; 14]); // rest of the GUID

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((20 + fmt.len() + data.len()) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&fmt);
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&data);

        let pcm = parse_wav(&bytes).expect("extensible PCM should parse");
        assert!((pcm.samples[0] - 0.5).abs() < 1e-6);
    }

    // ── Error paths ───────────────────────────────────────────────────────────

    #[test]
    fn headerless_input_errors_without_panicking() {
        let err = parse_wav(&[0u8; 64]).expect_err("random bytes are not a WAV");
        assert!(matches!(err, AudioError::NotWav(_)), "got {err:?}");
        assert!(err.to_string().contains("RIFF"));
    }

    #[test]
    fn empty_input_errors_without_panicking() {
        let err = parse_wav(&[]).expect_err("empty input is not a WAV");
        assert!(matches!(err, AudioError::NotWav(_)), "got {err:?}");
    }

    #[test]
    fn truncated_header_errors_without_panicking() {
        let err = parse_wav(b"RIFF").expect_err("4 bytes is not a WAV");
        assert!(matches!(err, AudioError::NotWav(_)), "got {err:?}");
    }

    #[test]
    fn missing_data_chunk_errors() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&28u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&WAVE_FORMAT_PCM.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&32_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        let err = parse_wav(&bytes).expect_err("no data chunk");
        assert!(err.to_string().contains("data"), "got {err}");
    }

    #[test]
    fn unsupported_bit_depth_errors_clearly() {
        let bytes = build_wav(WAVE_FORMAT_PCM, 1, 16_000, 12, &[0u8; 4]);
        let err = parse_wav(&bytes).expect_err("12-bit PCM is not supported");
        assert!(
            matches!(err, AudioError::UnsupportedSampleFormat(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn mp3_extension_reports_no_decoder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp3");
        std::fs::write(&path, b"ID3\x04\x00\x00\x00\x00\x00\x00").unwrap();
        let err = load_pcm_at(&path, 16_000).expect_err("mp3 has no decoder");
        assert!(
            matches!(err, AudioError::UnsupportedFormat { .. }),
            "got {err:?}"
        );
        assert!(
            err.to_string().contains("mp3 not supported: no decoder"),
            "message should name the format: {err}"
        );
    }

    #[test]
    fn missing_file_errors() {
        let err = load_pcm_at(Path::new("/tmp/definitely_not_here_xyz.wav"), 16_000)
            .expect_err("missing file");
        assert!(matches!(err, AudioError::Io(_, _)), "got {err:?}");
    }

    // ── Resampling ────────────────────────────────────────────────────────────

    #[test]
    fn resample_same_rate_is_identity() {
        let s = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&s, 16_000, 16_000), s);
    }

    #[test]
    fn resample_halving_rate_halves_length() {
        let s: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        let out = resample_linear(&s, 32_000, 16_000);
        assert_eq!(out.len(), 50);
    }

    #[test]
    fn resample_doubling_rate_doubles_length() {
        let s: Vec<f32> = (0..50).map(|i| i as f32).collect();
        let out = resample_linear(&s, 8_000, 16_000);
        assert_eq!(out.len(), 100);
    }

    #[test]
    fn resample_interpolates_midpoints_when_doubling() {
        let s = vec![0.0, 1.0];
        let out = resample_linear(&s, 8_000, 16_000);
        assert_eq!(out.len(), 4);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6, "midpoint should interpolate");
        assert!((out[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn resample_preserves_a_constant_signal() {
        let s = vec![0.42f32; 64];
        for out in resample_linear(&s, 44_100, 16_000) {
            assert!((out - 0.42).abs() < 1e-6);
        }
    }

    #[test]
    fn resample_empty_input_returns_empty() {
        assert!(resample_linear(&[], 44_100, 16_000).is_empty());
    }

    #[test]
    fn resample_zero_rate_is_a_no_op() {
        let s = vec![1.0, 2.0];
        assert_eq!(resample_linear(&s, 0, 16_000), s);
        assert_eq!(resample_linear(&s, 16_000, 0), s);
    }

    // ── Byte serialisation ────────────────────────────────────────────────────

    #[test]
    fn to_f32_le_bytes_emits_four_bytes_per_sample() {
        let bytes = to_f32_le_bytes(&[1.0, -1.0, 0.0]);
        assert_eq!(bytes.len(), 12);
        assert_eq!(&bytes[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&bytes[4..8], &(-1.0f32).to_le_bytes());
    }

    #[test]
    fn to_f32_le_bytes_round_trips() {
        let samples = vec![0.25f32, -0.5, 0.75];
        let bytes = to_f32_le_bytes(&samples);
        let back: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(back, samples);
    }

    // ── File-level helpers ────────────────────────────────────────────────────

    #[test]
    fn load_audio_data_url_embeds_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.wav");
        let bytes = wav_i16_mono(22_050, &[100, -100, 200]);
        std::fs::write(&path, &bytes).unwrap();

        let url = load_audio_data_url(&path).unwrap();
        assert!(url.starts_with("data:audio/wav;base64,"));
        let (mime, decoded) = parse_data_url(&url).unwrap();
        assert_eq!(mime, "audio/wav");
        assert_eq!(decoded, bytes, "payload must be the untouched file bytes");
    }

    #[test]
    fn load_audio_data_url_rejects_non_wav_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bogus.wav");
        std::fs::write(&path, b"not a wav at all").unwrap();
        assert!(load_audio_data_url(&path).is_err());
    }

    #[test]
    fn load_pcm_at_resamples_to_target_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.wav");
        // 1 second of 32 kHz audio → 16 kHz should yield 16 000 samples.
        std::fs::write(&path, wav_i16_mono(32_000, &vec![0i16; 32_000])).unwrap();
        let pcm = load_pcm_at(&path, 16_000).unwrap();
        assert_eq!(pcm.sample_rate, 16_000);
        assert_eq!(pcm.samples.len(), 16_000);
    }

    #[test]
    fn probe_reports_rate_channels_and_duration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stereo.wav");
        let mut data = Vec::new();
        for _ in 0..16_000 {
            data.extend_from_slice(&0i16.to_le_bytes());
            data.extend_from_slice(&0i16.to_le_bytes());
        }
        std::fs::write(&path, build_wav(WAVE_FORMAT_PCM, 2, 16_000, 16, &data)).unwrap();
        let spec = probe(&path).unwrap();
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.channels, 2);
        assert!((spec.duration_secs - 1.0).abs() < 1e-3);
    }

    #[test]
    fn oversized_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.wav");
        std::fs::write(&path, vec![0u8; MAX_AUDIO_BYTES + 1]).unwrap();
        let err = load_pcm_at(&path, 16_000).expect_err("over the cap");
        assert!(matches!(err, AudioError::TooLarge { .. }), "got {err:?}");
    }

    // ── Extension classification ──────────────────────────────────────────────

    #[test]
    fn is_audio_extension_recognises_known_formats() {
        for ext in &["wav", "WAV", "mp3", "m4a", "flac", "ogg"] {
            assert!(is_audio_extension(ext), "{ext} should be recognised");
        }
    }

    #[test]
    fn is_audio_extension_rejects_unknown() {
        for ext in &["png", "rs", "txt", ""] {
            assert!(!is_audio_extension(ext), "{ext} should not be audio");
        }
    }

    // ── Data URLs ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_data_url_rejects_non_data_url() {
        assert!(matches!(
            parse_data_url("https://example.com/a.wav"),
            Err(AudioError::InvalidDataUrl(_))
        ));
    }

    #[test]
    fn parse_data_url_rejects_bad_base64() {
        assert!(matches!(
            parse_data_url("data:audio/wav;base64,!!!!not base64!!!!"),
            Err(AudioError::Base64(_))
        ));
    }
}
