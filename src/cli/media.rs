// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::path::PathBuf;

use clap::Subcommand;

/// `sven media` subcommands: check and join video clips without decoding
/// them, for scripts and harness checks.
#[derive(Subcommand, Debug)]
pub enum MediaCommands {
    /// Read an MP4 clip and check it is whole: its length, size, codec,
    /// frames and sound. Exits 1 when it is not a usable MP4 or is outside
    /// the bounds given.
    ///
    ///   sven media probe shot.mp4 --seconds 5 --json
    Probe {
        /// The clip.
        file: PathBuf,
        /// Refuse a clip whose length is not this many seconds, give or take
        /// `--tolerance`: a model renders close to the length it was asked
        /// for, rarely exactly.
        #[arg(long, value_name = "SECS")]
        seconds: Option<f64>,
        /// How far from `--seconds` a clip may be.
        #[arg(long, value_name = "SECS", default_value_t = 0.5, requires = "seconds")]
        tolerance: f64,
        /// Refuse a clip shorter than this many seconds.
        #[arg(long, value_name = "SECS")]
        min_seconds: Option<f64>,
        /// Refuse a clip longer than this many seconds.
        #[arg(long, value_name = "SECS")]
        max_seconds: Option<f64>,
        /// Refuse a clip whose picture is not this many pixels wide.
        #[arg(long, value_name = "PIXELS")]
        width: Option<u32>,
        /// Refuse a clip whose picture is not this many pixels high.
        #[arg(long, value_name = "PIXELS")]
        height: Option<u32>,
        /// Refuse a clip that does not hold exactly this many frames.
        #[arg(long, value_name = "COUNT")]
        frames: Option<u64>,
        /// Refuse a clip whose frame rate is not this many frames per second,
        /// give or take 0.01.
        #[arg(long, value_name = "FPS")]
        fps: Option<f64>,
        /// Refuse a clip without sound.
        #[arg(long)]
        require_audio: bool,
        /// Print what the clip holds as one JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Join MP4 clips one after another into one file, copying them as they
    /// are (no re-encoding: the clips must come from the same model at the
    /// same size). Sound stays in sync with each clip's picture.
    ///
    ///   sven media join --output film.mp4 shots/01.mp4 shots/02.mp4
    ///   sven media join --output film.mp4 --dir shots --list shots.json
    Join {
        /// The file to write (replaced whole, or left untouched on failure).
        #[arg(long, value_name = "FILE")]
        output: PathBuf,
        /// A JSON array of the clips, in order (`-` reads standard input):
        /// each a path, or an object whose `id` names `<dir>/<id>.mp4`.
        /// Clips given as arguments come after it.
        #[arg(long, value_name = "FILE")]
        list: Option<PathBuf>,
        /// Where the clips an object of `--list` names are.
        #[arg(long, value_name = "DIR", default_value = ".")]
        dir: PathBuf,
        /// Leave the sound out.
        #[arg(long)]
        video_only: bool,
        /// Print what was written as one JSON object.
        #[arg(long)]
        json: bool,
        /// The clips, in order.
        clips: Vec<PathBuf>,
    },
}
