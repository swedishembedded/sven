// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for assembling generated clips into
// finished films for its clients. If your team needs expertise in generative
// media pipelines then you can procure our services by sending an email to
// info@swedishembedded.com.
//! `sven media`: the clip checks and the join of `sven_tools_media::mp4`, for
//! scripts and harness checks.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;
use sven_tools_media::mp4::{self, Probe, VideoInfo};

use crate::cli::MediaCommands;

pub(crate) fn run_media_command(command: &MediaCommands) -> Result<()> {
    match command {
        MediaCommands::Probe {
            file,
            seconds: expected,
            tolerance,
            min_seconds,
            max_seconds,
            width,
            height,
            frames,
            fps,
            require_audio,
            json,
        } => {
            let probe = mp4::probe(file)?;
            let seconds = probe.duration_seconds;
            if let Some(expected) = expected.filter(|&e| (seconds - e).abs() > *tolerance) {
                bail!(
                    "{} lasts {seconds:.2} s, not {expected} s (give or take {tolerance} s)",
                    file.display()
                );
            }
            if let Some(min) = min_seconds.filter(|&min| seconds < min) {
                bail!("{} lasts {seconds:.2} s, less than {min} s", file.display());
            }
            if let Some(max) = max_seconds.filter(|&max| seconds > max) {
                bail!("{} lasts {seconds:.2} s, more than {max} s", file.display());
            }
            let wanted = PictureWanted {
                width: *width,
                height: *height,
                frames: *frames,
                fps: *fps,
            };
            if let Some(misses) = wanted.misses(&probe.video) {
                bail!("{} {misses}", file.display());
            }
            if *require_audio && probe.audio.is_none() {
                bail!("{} has no sound", file.display());
            }
            print(&probe, *json)
        }
        MediaCommands::Join {
            output,
            list,
            dir,
            video_only,
            json,
            clips,
        } => {
            let mut all = match list {
                Some(list) => listed(list, dir)?,
                None => Vec::new(),
            };
            all.extend(clips.iter().cloned());
            let probe = mp4::join(&all, output, *video_only)?;
            print(&probe, *json)
        }
    }
}

/// How far a clip's frame rate may be from `--fps`: the rate is averaged over
/// the track, so it is never exactly the nominal one.
const FPS_TOLERANCE: f64 = 0.01;

/// What `sven media probe` was told the picture must be exactly.
#[derive(Debug, Default)]
struct PictureWanted {
    width: Option<u32>,
    height: Option<u32>,
    frames: Option<u64>,
    fps: Option<f64>,
}

impl PictureWanted {
    /// Every way `video` differs from what was asked, expected against actual,
    /// or none when it is as asked.
    fn misses(&self, video: &VideoInfo) -> Option<String> {
        let mut misses = Vec::new();
        if let Some(width) = self.width.filter(|&w| w != video.width) {
            misses.push(format!("is {} px wide, not {width}", video.width));
        }
        if let Some(height) = self.height.filter(|&h| h != video.height) {
            misses.push(format!("is {} px high, not {height}", video.height));
        }
        if let Some(frames) = self.frames.filter(|&f| f != video.frames) {
            misses.push(format!("holds {} frames, not {frames}", video.frames));
        }
        if let Some(fps) = self.fps.filter(|&f| (f - video.fps).abs() > FPS_TOLERANCE) {
            misses.push(format!(
                "runs at {:.3} fps, not {fps} (give or take {FPS_TOLERANCE})",
                video.fps
            ));
        }
        (!misses.is_empty()).then(|| misses.join(", "))
    }
}

/// The clips a `--list` names, in order.
fn listed(list: &Path, dir: &Path) -> Result<Vec<PathBuf>> {
    let mut text = String::new();
    if list == Path::new("-") {
        std::io::stdin().read_to_string(&mut text)?;
    } else {
        text =
            std::fs::read_to_string(list).with_context(|| format!("reading {}", list.display()))?;
    }
    let value: Value = serde_json::from_str(&text).context("--list is not JSON")?;
    let Value::Array(items) = value else {
        bail!("--list must be a JSON array of clips");
    };
    items
        .iter()
        .map(|item| match item {
            Value::String(path) => Ok(PathBuf::from(path)),
            Value::Object(fields) => match fields.get("id") {
                Some(Value::String(id)) if is_file_name(id) => Ok(dir.join(format!("{id}.mp4"))),
                Some(Value::Number(id)) => Ok(dir.join(format!("{id}.mp4"))),
                _ => bail!("an object of --list needs an `id` that is a plain file name"),
            },
            _ => bail!("--list holds paths or objects with an `id`"),
        })
        .collect()
}

/// An id that names a file in the directory, never one elsewhere.
fn is_file_name(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\'])
}

fn print(probe: &Probe, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(probe)?);
    } else {
        let v = &probe.video;
        let sound = probe.audio.as_ref().map_or_else(
            || "no sound".to_owned(),
            |a| format!("sound {} {} Hz", a.codec, a.sample_rate),
        );
        println!(
            "{:.2} s, {}x{} {}, {} frames at {:.2} fps, {sound}, {} bytes",
            probe.duration_seconds, v.width, v.height, v.codec, v.frames, v.fps, probe.bytes
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video() -> VideoInfo {
        VideoInfo {
            codec: "av01".into(),
            width: 832,
            height: 480,
            frames: 121,
            fps: 24.0,
        }
    }

    #[test]
    fn a_picture_as_asked_passes_and_any_difference_names_expected_and_actual() {
        let ask = |width, height, frames, fps| PictureWanted {
            width,
            height,
            frames,
            fps,
        };
        assert_eq!(ask(None, None, None, None).misses(&video()), None);
        assert_eq!(
            ask(Some(832), Some(480), Some(121), Some(24.005)).misses(&video()),
            None
        );
        assert_eq!(
            ask(Some(1280), Some(720), Some(129), Some(25.0))
                .misses(&video())
                .unwrap(),
            "is 832 px wide, not 1280, is 480 px high, not 720, holds 121 frames, not 129, \
             runs at 24.000 fps, not 25 (give or take 0.01)"
        );
        assert!(ask(None, None, None, Some(24.02))
            .misses(&video())
            .is_some());
    }
}
