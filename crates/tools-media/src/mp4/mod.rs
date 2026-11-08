// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for assembling generated clips into
// finished films for its clients. If your team needs expertise in media
// container formats then you can procure our services by sending an email to
// info@swedishembedded.com.
//! MP4 clips read, checked and joined without decoding a frame.
//!
//! [`probe`] reads what a file holds (its length, size, codec, frames and
//! sound) and checks that every sample it lists is really in the file, so a
//! truncated download or a file that is not a video is caught before anything
//! builds on it. [`join`] puts clips one after another into one file by
//! copying their samples: any codec works, and nothing is re-encoded, so the
//! clips must be encoded alike (the same model at the same size gives that).
//!
//! **Sound stays in sync.** Each clip's own edit list (an encoder's start
//! delay, a sound track's priming) is carried into the joined file, shifted to
//! where that clip's samples now are, and every track of a clip is cut or
//! padded with silence to that clip's picture length. A sound track a little
//! longer than its picture therefore never drifts across a film of hundreds
//! of shots.
//!
//! Only the first picture track and the first sound track of a clip are
//! joined; other tracks (timecode, subtitles) are left out. Fragmented MP4 and
//! clips with several sample descriptions in one track are refused.

mod boxes;
mod read;
mod write;

use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Result};
use serde::Serialize;

use read::{Kind, Movie};
use write::{write_joined, Joined};

/// What a clip holds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Probe {
    /// How long it plays, in seconds.
    pub duration_seconds: f64,
    /// The file's length in bytes.
    pub bytes: u64,
    pub video: VideoInfo,
    pub audio: Option<AudioInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VideoInfo {
    /// The sample description's four-character code (`av01`, `avc1`, ...).
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    /// Frames per second, averaged over the track.
    pub fps: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioInfo {
    /// The sample description's four-character code (`Opus`, `mp4a`, ...).
    pub codec: String,
    pub sample_rate: u32,
}

/// Reads and checks the clip at `path`.
pub fn probe(path: &Path) -> Result<Probe> {
    let movie = Movie::read(path)?;
    let video = movie.video()?;
    let (width, height) = video.size;
    let seconds =
        video.presented(movie.timescale, movie.timescale) as f64 / f64::from(movie.timescale);
    let media_seconds = video.media_duration() as f64 / f64::from(video.timescale);
    Ok(Probe {
        duration_seconds: seconds,
        bytes: movie.bytes,
        video: VideoInfo {
            codec: video.codec(),
            width,
            height,
            frames: video.samples.len() as u64,
            fps: if media_seconds > 0.0 {
                video.samples.len() as f64 / media_seconds
            } else {
                0.0
            },
        },
        audio: movie.audio().map(|audio| AudioInfo {
            codec: audio.codec(),
            sample_rate: audio.timescale,
        }),
    })
}

/// Joins `clips`, in order, into one file at `output` (written whole or not
/// at all), dropping their sound when `video_only`. Answers what it wrote.
pub fn join(clips: &[PathBuf], output: &Path, video_only: bool) -> Result<Probe> {
    ensure!(!clips.is_empty(), "nothing to join: name at least one clip");
    let movies = clips
        .iter()
        .map(|path| Movie::read(path))
        .collect::<Result<Vec<_>>>()?;
    let first = &movies[0];
    let first_video = first.video()?;
    let with_audio = !video_only && first.audio().is_some();
    for (index, movie) in movies.iter().enumerate().skip(1) {
        let name = clips[index].display();
        let video = movie.video()?;
        ensure!(
            video.stsd == first_video.stsd && video.timescale == first_video.timescale,
            "{name} is encoded differently from {} ({} {}x{} against {} {}x{}): joining copies the clips as they are, so they must come from the same model at the same size",
            clips[0].display(),
            video.codec(), video.size.0, video.size.1,
            first_video.codec(), first_video.size.0, first_video.size.1,
        );
        if video_only {
            continue;
        }
        match (first.audio(), movie.audio()) {
            (Some(a), Some(b)) => ensure!(
                a.stsd == b.stsd && a.timescale == b.timescale,
                "the sound of {name} is encoded differently from that of {}",
                clips[0].display()
            ),
            (None, None) => {}
            _ => bail!(
                "{name} and {} differ in having sound; make every clip with or without it, or join the pictures only",
                clips[0].display()
            ),
        }
    }
    let timescale = first.timescale;
    let lengths: Vec<u64> = movies
        .iter()
        .map(|movie| Ok(movie.video()?.presented(timescale, movie.timescale)))
        .collect::<Result<_>>()?;
    let mut tracks = vec![Joined::new(&movies, &lengths, timescale, Kind::Video)?];
    if with_audio {
        tracks.push(Joined::new(&movies, &lengths, timescale, Kind::Audio)?);
    }
    write_joined(&movies, first, &tracks, output)?;
    probe(output)
}

#[cfg(any(test, feature = "fake-server"))]
pub mod fixture;
#[cfg(test)]
pub(crate) mod tests;
