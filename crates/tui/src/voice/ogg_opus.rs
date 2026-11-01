// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for compact speech transport for
// its clients. If your team needs expertise in Opus and Ogg encoding for
// voice interfaces then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Mono 16 kHz samples as an Ogg/Opus file (RFC 7845), 24 kbit/s, 20 ms frames.

use anyhow::{anyhow, Context};
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Bitrate, Channels, Encoder};

use super::windower::{FRAME_SAMPLES, SAMPLE_RATE};

const BITRATE_BPS: i32 = 24_000;
/// Opus counts granule positions in 48 kHz samples whatever the input rate.
const GRANULE_PER_SAMPLE: u64 = 48_000 / SAMPLE_RATE as u64;
/// Fixed serial of the single logical stream in the file.
const STREAM_SERIAL: u32 = 0x5356_4e31;
const MAX_PACKET_BYTES: usize = 1275;

/// Encodes `samples` (16 kHz mono, -1.0 to 1.0) as an Ogg/Opus file.
///
/// # Errors
/// If `samples` is empty or the encoder refuses its configuration.
pub fn encode(samples: &[f32]) -> anyhow::Result<Vec<u8>> {
    if samples.is_empty() {
        return Err(anyhow!("no audio to encode"));
    }
    let mut encoder = Encoder::new(SAMPLE_RATE as u32, Channels::Mono, Application::Voip)
        .context("opus encoder")?;
    encoder
        .set_bitrate(Bitrate::Bits(BITRATE_BPS))
        .context("opus bitrate")?;
    let lookahead = u64::try_from(encoder.get_lookahead().context("opus lookahead")?)
        .context("opus lookahead")?;
    let pre_skip = lookahead * GRANULE_PER_SAMPLE;

    let mut writer = PacketWriter::new(Vec::new());
    writer
        .write_packet(
            head(pre_skip as u16),
            STREAM_SERIAL,
            PacketWriteEndInfo::EndPage,
            0,
        )
        .context("ogg head")?;
    writer
        .write_packet(tags(), STREAM_SERIAL, PacketWriteEndInfo::EndPage, 0)
        .context("ogg tags")?;

    // The encoder delays its output by `lookahead` samples, so feed that many
    // zeros after the signal to get every real sample out; the decoder drops
    // the delay from the front (pre-skip) and the padding from the back (the
    // last granule position).
    let total = samples.len() as u64;
    let packets = (total + lookahead).div_ceil(FRAME_SAMPLES as u64);
    let mut frame = [0.0f32; FRAME_SAMPLES];
    for n in 0..packets {
        let from = (n as usize) * FRAME_SAMPLES;
        let have = samples.len().saturating_sub(from).min(FRAME_SAMPLES);
        frame[..have].copy_from_slice(&samples[from..from + have]);
        frame[have..].fill(0.0);
        let packet = encoder
            .encode_vec_float(&frame, MAX_PACKET_BYTES)
            .context("opus encode")?;
        let last = n + 1 == packets;
        let (end, granule) = if last {
            (
                PacketWriteEndInfo::EndStream,
                pre_skip + total * GRANULE_PER_SAMPLE,
            )
        } else {
            (PacketWriteEndInfo::NormalPacket, (n + 1) * 960)
        };
        writer
            .write_packet(packet, STREAM_SERIAL, end, granule)
            .context("ogg audio")?;
    }
    Ok(writer.into_inner())
}

/// The OpusHead packet: one mono stream, channel mapping family 0.
fn head(pre_skip: u16) -> Vec<u8> {
    let mut h = Vec::with_capacity(19);
    h.extend_from_slice(b"OpusHead");
    h.push(1); // version
    h.push(1); // channels
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&(SAMPLE_RATE as u32).to_le_bytes()); // input rate, informative
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    h.push(0); // mapping family
    h
}

/// The OpusTags packet: a vendor string and no comments.
fn tags() -> Vec<u8> {
    let vendor = b"sven";
    let mut t = Vec::new();
    t.extend_from_slice(b"OpusTags");
    t.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    t.extend_from_slice(vendor);
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogg::reading::PacketReader;

    fn tone(seconds: f32) -> Vec<f32> {
        (0..(seconds * SAMPLE_RATE as f32) as usize)
            .map(|i| 0.3 * (std::f32::consts::TAU * 440.0 * i as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Decodes a file the way a conforming reader does: drop the pre-skip,
    /// stop at the last granule position.
    fn decode(file: &[u8]) -> (Vec<f32>, Vec<u8>, Vec<u8>) {
        let mut reader = PacketReader::new(std::io::Cursor::new(file));
        let head = reader.read_packet_expected().unwrap();
        let tags = reader.read_packet_expected().unwrap();
        assert!(head.data.starts_with(b"OpusHead"));
        assert!(tags.data.starts_with(b"OpusTags"));
        let pre_skip = u16::from_le_bytes([head.data[10], head.data[11]]) as usize;
        let mut decoder = opus::Decoder::new(48_000, Channels::Mono).unwrap();
        let mut pcm = Vec::new();
        let mut last_granule = 0;
        while let Some(packet) = reader.read_packet().unwrap() {
            let mut out = vec![0.0f32; 5760];
            let n = decoder.decode_float(&packet.data, &mut out, false).unwrap();
            pcm.extend_from_slice(&out[..n]);
            last_granule = packet.absgp_page();
        }
        pcm.truncate(last_granule as usize);
        (pcm[pre_skip..].to_vec(), head.data, tags.data)
    }

    #[test]
    fn a_tone_comes_back_with_its_duration_and_energy() {
        let input = tone(1.3);
        let file = encode(&input).unwrap();
        assert!(file.starts_with(b"OggS"));
        // 24 kbit/s is 3 kB per second; allow generous slack either way.
        assert!(file.len() < 6_000, "{} bytes", file.len());

        let (pcm, head, _) = decode(&file);
        assert_eq!(head[9], 1, "mono");
        assert_eq!(&head[12..16], &16_000u32.to_le_bytes());
        let seconds = pcm.len() as f32 / 48_000.0;
        assert!((seconds - 1.3).abs() < 0.001, "duration {seconds}");
        let ratio = rms(&pcm) / rms(&input);
        assert!((0.8..1.2).contains(&ratio), "energy ratio {ratio}");
    }

    #[test]
    fn a_length_that_is_not_a_whole_frame_is_trimmed_exactly() {
        let input = tone(1.0)[..8_123].to_vec();
        let (pcm, _, _) = decode(&encode(&input).unwrap());
        assert_eq!(pcm.len(), input.len() * 3);
    }

    #[test]
    fn nothing_to_encode_is_an_error() {
        assert!(encode(&[]).is_err());
    }
}
