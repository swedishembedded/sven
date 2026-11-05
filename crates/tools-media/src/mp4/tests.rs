// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use super::boxes::{boxed, full_box};
use super::read::{Edit, Kind, Movie, RATE_ONE};
use super::*;

/// What a fixture clip holds: frames of picture at `fps` (timescale `fps *
/// 100`) and, optionally, sound packets of 20 ms at 48 kHz with a priming
/// edit like an Opus encoder's.
pub(crate) struct Clip {
    codec: [u8; 4],
    config: Vec<u8>,
    size: (u32, u32),
    frames: Vec<Vec<u8>>,
    fps: u32,
    sound: Option<Vec<Vec<u8>>>,
    pre_skip: u32,
}

impl Clip {
    pub(crate) fn new(tag: u8, frames: usize) -> Self {
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
    pub(crate) fn with_sound(mut self, tag: u8) -> Self {
        let seconds = self.frames.len() as f64 / f64::from(self.fps);
        let packets = (seconds / 0.02).ceil() as usize + 2;
        self.sound = Some((0..packets).map(|i| vec![tag, 0x50, i as u8]).collect());
        self.pre_skip = 312;
        self
    }

    pub(crate) fn write(&self, path: &Path) {
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
        std::fs::write(path, [ftyp, moov(base), boxed(b"mdat", &mdat)].concat()).unwrap();
    }
}

/// The bytes of every sample of a track of the file at `path`, in order.
fn samples(path: &Path, kind: Kind) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).unwrap();
    let movie = Movie::read(path).unwrap();
    movie
        .track(kind)
        .unwrap()
        .samples
        .iter()
        .map(|s| bytes[s.offset as usize..s.offset as usize + s.size as usize].to_vec())
        .collect()
}

#[test]
fn a_probe_reads_what_a_clip_holds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.mp4");
    Clip::new(1, 48).with_sound(1).write(&path);
    let probe = probe(&path).unwrap();
    assert!((probe.duration_seconds - 2.0).abs() < 1e-9, "{probe:?}");
    assert_eq!(probe.video.codec, "av01");
    assert_eq!((probe.video.width, probe.video.height), (64, 48));
    assert_eq!(probe.video.frames, 48);
    assert!((probe.video.fps - 24.0).abs() < 1e-9);
    assert_eq!(probe.audio.unwrap().sample_rate, 48_000);
}

#[test]
fn a_probe_refuses_a_cut_short_download_and_a_file_that_is_no_video() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.mp4");
    Clip::new(1, 24).write(&path);
    let whole = std::fs::read(&path).unwrap();
    std::fs::write(&path, &whole[..whole.len() - 5]).unwrap();
    let error = format!("{:#}", probe(&path).unwrap_err());
    assert!(error.contains("cut short"), "{error}");
    std::fs::write(&path, b"<html>not found</html>").unwrap();
    assert!(probe(&path).is_err());
}

#[test]
fn joined_clips_play_one_after_another_with_every_frame_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<PathBuf> = (0..3)
        .map(|i| dir.path().join(format!("{i}.mp4")))
        .collect();
    for (i, path) in paths.iter().enumerate() {
        Clip::new(i as u8, 24 * (i + 1)).write(path);
    }
    let out = dir.path().join("film.mp4");
    let probe = join(&paths, &out, false).unwrap();
    assert!((probe.duration_seconds - 6.0).abs() < 1e-9, "{probe:?}");
    assert_eq!(probe.video.frames, 24 * 6);
    let expected: Vec<Vec<u8>> = paths.iter().flat_map(|p| samples(p, Kind::Video)).collect();
    assert_eq!(samples(&out, Kind::Video), expected);
    assert!(probe.audio.is_none());
    // Nothing is left beside the film.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
}

#[test]
fn sound_is_cut_to_each_clips_picture_so_it_never_drifts() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<PathBuf> = (0..2)
        .map(|i| dir.path().join(format!("{i}.mp4")))
        .collect();
    Clip::new(0, 24).with_sound(0).write(&paths[0]);
    Clip::new(1, 36).with_sound(1).write(&paths[1]);
    let out = dir.path().join("film.mp4");
    join(&paths, &out, false).unwrap();
    let movie = Movie::read(&out).unwrap();
    let audio = movie.audio().unwrap();
    let expected: Vec<Vec<u8>> = paths.iter().flat_map(|p| samples(p, Kind::Audio)).collect();
    assert_eq!(samples(&out, Kind::Audio), expected);
    // Each clip's sound starts after its own priming and lasts exactly as
    // long as its picture (1 s, then 1.5 s), so the second clip's sound
    // starts with its picture.
    let edits = audio.edits.clone().unwrap();
    let first_media = Movie::read(&paths[0])
        .unwrap()
        .audio()
        .unwrap()
        .media_duration() as i64;
    assert_eq!(
        edits,
        vec![
            Edit {
                duration: 1000,
                media_time: 312,
                rate: RATE_ONE
            },
            Edit {
                duration: 1500,
                media_time: first_media + 312,
                rate: RATE_ONE
            },
        ]
    );
    let video = movie.video().unwrap();
    assert_eq!(
        audio.presented(movie.timescale, movie.timescale),
        video.presented(movie.timescale, movie.timescale)
    );
}

#[test]
fn clips_encoded_differently_are_refused_naming_the_clip() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.mp4"), dir.path().join("b.mp4"));
    Clip::new(0, 24).write(&a);
    let mut other = Clip::new(1, 24);
    other.size = (128, 96);
    other.write(&b);
    let error = format!(
        "{:#}",
        join(&[a.clone(), b.clone()], &dir.path().join("f.mp4"), false).unwrap_err()
    );
    assert!(
        error.contains("b.mp4") && error.contains("same model at the same size"),
        "{error}"
    );

    Clip::new(1, 24).with_sound(1).write(&b);
    let error = format!(
        "{:#}",
        join(&[a.clone(), b.clone()], &dir.path().join("f.mp4"), false).unwrap_err()
    );
    assert!(error.contains("sound"), "{error}");
    // The pictures alone join.
    let probe = join(&[a, b], &dir.path().join("f.mp4"), true).unwrap();
    assert!(probe.audio.is_none());
    assert_eq!(probe.video.frames, 48);
}
