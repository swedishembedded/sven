// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::path::PathBuf;

use clap::{Args, Subcommand};

/// `sven media` subcommands: make, check and join video clips, for scripts and
/// harness checks.
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
    /// Make a clip with a video model: the request the `run_model` tool makes,
    /// as a command a script or a harness gate can run without a model turn.
    /// The provider is the one `tools.media.provider` names (default: the
    /// sven service), with its key from `providers.<name>` or `sven auth`.
    ///
    /// The same request twice delivers the clip made the first time and
    /// submits nothing; a run killed while the server renders asks again
    /// under the same name and picks the job up where the server has it.
    /// Progress goes to stderr; stdout holds the path written (one JSON
    /// object with `--json`).
    ///
    ///   sven media generate --model brain/minimaxh3 --action ref2va \
    ///     --prompt "<Picture 1> waves" --reference image:face.png \
    ///     --size 768x448 --param num_frames=121 --output shot.mp4
    Generate(GenerateArgs),
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

/// The request of `sven media generate`: the arguments of `run_model`.
#[derive(Args, Debug)]
pub struct GenerateArgs {
    /// The id of a video model (`brain/minimaxh3`).
    #[arg(long, value_name = "ID")]
    pub model: String,
    /// What the video shows. Names the references by their order within their
    /// kind: `<Picture 1>`, `<Video 1>`, `<Audio 1>`.
    #[arg(long, value_name = "TEXT")]
    pub prompt: String,
    /// Seconds.
    #[arg(long, value_name = "SECS", value_parser = number)]
    pub duration: Option<serde_json::Number>,
    /// 480p, 720p, 1080p, ...
    #[arg(long, value_name = "NAME")]
    pub resolution: Option<String>,
    /// 16:9, 9:16, 1:1, ...
    #[arg(long, value_name = "W:H")]
    pub aspect_ratio: Option<String>,
    /// Exact pixels, WIDTHxHEIGHT.
    #[arg(long, value_name = "WxH")]
    pub size: Option<String>,
    /// The seed: another seed is another take.
    #[arg(long, value_name = "N", value_parser = number)]
    pub seed: Option<serde_json::Number>,
    /// Ask for sound.
    #[arg(long)]
    pub generate_audio: bool,
    /// An action the model offers for its references (`ref2va`), not an edit.
    #[arg(long, value_name = "NAME", conflicts_with = "edit")]
    pub action: Option<String>,
    /// One of the action's or edit's own parameters (repeatable). VALUE is
    /// read as JSON when it is JSON (`steps=30`, `allow_community=true`),
    /// else it is a string (`adapter=/models/lora`).
    #[arg(long = "param", value_name = "KEY=VALUE")]
    pub params: Vec<String>,
    /// A file the prompt refers to (repeatable, in the order the prompt
    /// counts them): `image|video|audio:PATH`, inside the current directory.
    #[arg(long = "reference", value_name = "KIND:PATH")]
    pub references: Vec<String>,
    /// An edit the model offers (`extend`, `upscale`), with `--input-video`.
    #[arg(long, value_name = "NAME", requires = "input_video")]
    pub edit: Option<String>,
    /// The clip to edit, inside the current directory.
    #[arg(long, value_name = "FILE", requires = "edit")]
    pub input_video: Option<PathBuf>,
    /// The file to write (a path inside the current directory; replaced whole,
    /// or left untouched on failure).
    #[arg(long, value_name = "FILE")]
    pub output: PathBuf,
    /// Print the clip as one JSON object: its path, SHA-256 digest, seconds,
    /// width, height and frames.
    #[arg(long)]
    pub json: bool,
}

/// A JSON number, so `5` and `5.0` stay what the person wrote: the request is
/// named by its canonical arguments.
fn number(text: &str) -> Result<serde_json::Number, String> {
    serde_json::from_str(text).map_err(|_| format!("`{text}` is not a number"))
}
