// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for testing generative-media
// pipelines end to end without a GPU for its clients. If your team needs
// expertise in media pipeline verification then you can procure our services
// by sending an email to info@swedishembedded.com.
//! Fixture MP4 clips: whole, readable files of any size and length, built
//! without an encoder. Their samples are placeholders, not decodable video,
//! so they stand in for a model's output wherever only the container is read
//! (`probe`, `join`).

use super::boxes::{boxed, full_box};
use super::*;

/// What a fixture clip holds: frames of picture at `fps` (timescale `fps *
/// 100`) and, optionally, sound packets of 20 ms at 48 kHz with a priming
/// edit like an Opus encoder's.
pub struct Clip {
    codec: [u8; 4],
    config: Vec<u8>,
    size: (u32, u32),
    frames: Vec<Vec<u8>>,
    fps: u32,
    sound: Option<Vec<Vec<u8>>>,
    pre_skip: u32,
}

impl Clip {
    #[must_use]
    pub fn new(tag: u8, frames: usize) -> Self {
        Self {
            codec: *b"av01",
            config: vec![0x81, 0x08, 0x0c, 0x00],
            size: (64, 48),
            frames: (0..frames).map(|i| vec![tag, i as u8, 0xAA]).collect(),
            fps: 24,
            sound: None,
            pre_skip: 0,
        }
    }

    /// Sound slightly longer than the picture, as encoders leave it.
    #[must_use]
    pub fn with_sound(mut self, tag: u8) -> Self {
        let seconds = self.frames.len() as f64 / f64::from(self.fps);
        let packets = (seconds / 0.02).ceil() as usize + 2;
        self.sound = Some((0..packets).map(|i| vec![tag, 0x50, i as u8]).collect());
        self.pre_skip = 312;
        self
    }

    /// A picture of `width` x `height` pixels.
    #[must_use]
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.size = (width, height);
        self
    }

    /// The file written at `path`.
    ///
    /// # Panics
    /// When the file cannot be written.
    pub fn write(&self, path: &Path) {
        std::fs::write(path, self.to_bytes()).unwrap();
    }

    /// The file's bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let ftyp = boxed(b"ftyp", b"isom\0\0\x02\0isomav01");
        let video_ts = self.fps * 100;
        let mut mdat = Vec::new();
        let mut video_offsets = Vec::new();
        let mut audio_offsets = Vec::new();
        // Picture and sound interleaved one sample each, to exercise chunk maps.
        let empty = Vec::new();
        let sound = self.sound.as_ref().unwrap_or(&empty);
        for i in 0..self.frames.len().max(sound.len()) {
            if let Some(f) = self.frames.get(i) {
                video_offsets.push(mdat.len() as u64);
                mdat.extend(f);
            }
            if let Some(p) = sound.get(i) {
                audio_offsets.push(mdat.len() as u64);
                mdat.extend(p);
            }
        }
        let video_media = self.frames.len() as u64 * 100;
        let video_ms = video_media * 1000 / u64::from(video_ts);
        let entry = [
            &[0u8; 6][..],
            &1u16.to_be_bytes(),
            &[0u8; 16],
            &(self.size.0 as u16).to_be_bytes(),
            &(self.size.1 as u16).to_be_bytes(),
            &[0u8; 50],
            &boxed(b"av1C", &self.config),
        ]
        .concat();
        let video_stbl = |base: u64| {
            [
                full_box(
                    b"stsd",
                    0,
                    0,
                    &[1u32.to_be_bytes().as_slice(), &boxed(&self.codec, &entry)].concat(),
                ),
                full_box(
                    b"stts",
                    0,
                    0,
                    &[
                        1u32.to_be_bytes(),
                        (self.frames.len() as u32).to_be_bytes(),
                        100u32.to_be_bytes(),
                    ]
                    .concat(),
                ),
                full_box(
                    b"stss",
                    0,
                    0,
                    &[1u32.to_be_bytes(), 1u32.to_be_bytes()].concat(),
                ),
                full_box(
                    b"stsc",
                    0,
                    0,
                    &[
                        1u32.to_be_bytes(),
                        1u32.to_be_bytes(),
                        1u32.to_be_bytes(),
                        1u32.to_be_bytes(),
                    ]
                    .concat(),
                ),
                full_box(
                    b"stsz",
                    0,
                    0,
                    &[
                        0u32.to_be_bytes().as_slice(),
                        &(self.frames.len() as u32).to_be_bytes(),
                        &self
                            .frames
                            .iter()
                            .flat_map(|f| (f.len() as u32).to_be_bytes())
                            .collect::<Vec<_>>(),
                    ]
                    .concat(),
                ),
                full_box(
                    b"co64",
                    0,
                    0,
                    &[
                        (video_offsets.len() as u32).to_be_bytes().as_slice(),
                        &video_offsets
                            .iter()
                            .flat_map(|o| (o + base).to_be_bytes())
                            .collect::<Vec<_>>(),
                    ]
                    .concat(),
                ),
            ]
            .concat()
        };
        let tkhd = |id: u32, ms: u64, w: u32, h: u32| {
            full_box(
                b"tkhd",
                0,
                3,
                &[
                    &[0u8; 8][..],
                    &id.to_be_bytes(),
                    &[0u8; 4],
                    &(ms as u32).to_be_bytes(),
                    &[0u8; 52],
                    &(w << 16).to_be_bytes(),
                    &(h << 16).to_be_bytes(),
                ]
                .concat(),
            )
        };
        let mdhd = |ts: u32, d: u64| {
            full_box(
                b"mdhd",
                0,
                0,
                &[
                    &[0u8; 8][..],
                    &ts.to_be_bytes(),
                    &(d as u32).to_be_bytes(),
                    &[0x55, 0xC4, 0, 0],
                ]
                .concat(),
            )
        };
        let hdlr = |k: &[u8; 4]| {
            full_box(
                b"hdlr",
                0,
                0,
                &[&[0u8; 4][..], k, &[0u8; 12], b"fixture\0"].concat(),
            )
        };
        let moov = |base: u64| {
            let mut moov = full_box(
                b"mvhd",
                0,
                0,
                &[
                    &[0u8; 8][..],
                    &1000u32.to_be_bytes(),
                    &(video_ms as u32).to_be_bytes(),
                    &[0u8; 76],
                    &3u32.to_be_bytes(),
                ]
                .concat(),
            );
            let minf = [
                full_box(b"vmhd", 0, 1, &[0u8; 8]),
                boxed(b"stbl", &video_stbl(base)),
            ]
            .concat();
            let mdia = [
                mdhd(video_ts, video_media),
                hdlr(b"vide"),
                boxed(b"minf", &minf),
            ]
            .concat();
            moov.extend(boxed(
                b"trak",
                &[
                    tkhd(1, video_ms, self.size.0, self.size.1),
                    boxed(b"mdia", &mdia),
                ]
                .concat(),
            ));
            if !sound.is_empty() {
                let media = sound.len() as u64 * 960;
                let entry = [
                    &[0u8; 6][..],
                    &1u16.to_be_bytes(),
                    &[0u8; 8],
                    &2u16.to_be_bytes(),
                    &16u16.to_be_bytes(),
                    &[0u8; 4],
                    &(48_000u32 << 16).to_be_bytes(),
                ]
                .concat();
                let stbl = [
                    full_box(
                        b"stsd",
                        0,
                        0,
                        &[1u32.to_be_bytes().as_slice(), &boxed(b"Opus", &entry)].concat(),
                    ),
                    full_box(
                        b"stts",
                        0,
                        0,
                        &[
                            1u32.to_be_bytes(),
                            (sound.len() as u32).to_be_bytes(),
                            960u32.to_be_bytes(),
                        ]
                        .concat(),
                    ),
                    full_box(
                        b"stsc",
                        0,
                        0,
                        &[
                            1u32.to_be_bytes(),
                            1u32.to_be_bytes(),
                            1u32.to_be_bytes(),
                            1u32.to_be_bytes(),
                        ]
                        .concat(),
                    ),
                    full_box(
                        b"stsz",
                        0,
                        0,
                        &[
                            0u32.to_be_bytes().as_slice(),
                            &(sound.len() as u32).to_be_bytes(),
                            &sound
                                .iter()
                                .flat_map(|p| (p.len() as u32).to_be_bytes())
                                .collect::<Vec<_>>(),
                        ]
                        .concat(),
                    ),
                    full_box(
                        b"stco",
                        0,
                        0,
                        &[
                            (audio_offsets.len() as u32).to_be_bytes().as_slice(),
                            &audio_offsets
                                .iter()
                                .flat_map(|o| ((o + base) as u32).to_be_bytes())
                                .collect::<Vec<_>>(),
                        ]
                        .concat(),
                    ),
                ]
                .concat();
                let audio_ms = (media - u64::from(self.pre_skip)) * 1000 / 48_000;
                let elst = full_box(
                    b"elst",
                    0,
                    0,
                    &[
                        1u32.to_be_bytes(),
                        (audio_ms as u32).to_be_bytes(),
                        self.pre_skip.to_be_bytes(),
                        0x0001_0000u32.to_be_bytes(),
                    ]
                    .concat(),
                );
                let minf = [full_box(b"smhd", 0, 0, &[0u8; 4]), boxed(b"stbl", &stbl)].concat();
                let mdia = [mdhd(48_000, media), hdlr(b"soun"), boxed(b"minf", &minf)].concat();
                moov.extend(boxed(
                    b"trak",
                    &[
                        tkhd(2, audio_ms, 0, 0),
                        boxed(b"edts", &elst),
                        boxed(b"mdia", &mdia),
                    ]
                    .concat(),
                ));
            }
            boxed(b"moov", &moov)
        };
        let base = ftyp.len() as u64 + moov(0).len() as u64 + 8;
        [ftyp, moov(base), boxed(b"mdat", &mdat)].concat()
    }
}
