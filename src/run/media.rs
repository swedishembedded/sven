// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for assembling generated clips into
// finished films for its clients. If your team needs expertise in generative
// media pipelines then you can procure our services by sending an email to
// info@swedishembedded.com.
//! `sven media`: the generation of `sven_tools_media::generate` and the clip
//! checks and join of `sven_tools_media::mp4`, for scripts and harness
//! checks.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use sven_tools_media::mp4::{self, Probe, VideoInfo};

use crate::cli::{GenerateArgs, MediaCommands};

pub(crate) async fn run_media_command(
    command: &MediaCommands,
    config: Option<&Path>,
) -> Result<()> {
    match command {
        MediaCommands::Generate(args) => generate(args, config).await,
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

/// `sven media generate`: the request `run_model` makes, for the current
/// directory, against the provider the configuration names for media.
async fn generate(args: &GenerateArgs, config: Option<&Path>) -> Result<()> {
    // The request is read before the configuration or the network is.
    let request = generate_request(args)?;
    let config = sven_frontend::Settings::load(config)?.runtime;
    let provider = config
        .tools
        .media
        .provider
        .clone()
        .unwrap_or_else(|| sven_model_drivers::sven_service::PROVIDER.to_owned());
    let (endpoint, media) = config.media_endpoint().with_context(|| {
        format!(
            "no key for the media provider `{provider}` (`tools.media.provider`): \
             store one with `sven auth set {provider}` or give `providers.{provider}.api_key_file`"
        )
    })?;
    let workdir = std::env::current_dir().context("the current directory")?;
    let done = sven_tools_media::generate(&endpoint, &media, &workdir, &request, &mut |line| {
        eprintln!("[sven:media] {line}");
    })
    .await
    .map_err(anyhow::Error::msg)?;
    if !args.json {
        println!("{}", done.shown);
        return Ok(());
    }
    let digest = done.digest()?;
    let clip =
        mp4::probe(&done.path).with_context(|| format!("{} is not a usable MP4", done.shown))?;
    println!(
        "{}",
        json!({
            "path": done.shown,
            "digest": digest,
            "seconds": clip.duration_seconds,
            "width": clip.video.width,
            "height": clip.video.height,
            "frames": clip.video.frames,
            "reused": done.reused,
        })
    );
    Ok(())
}

/// The arguments `run_model` takes for the flags given.
fn generate_request(args: &GenerateArgs) -> Result<Value> {
    let mut request = Map::new();
    request.insert("model".into(), json!(args.model));
    request.insert("prompt".into(), json!(args.prompt));
    for (key, value) in [
        ("duration", args.duration.as_ref().map(|n| json!(n))),
        ("resolution", args.resolution.as_ref().map(|v| json!(v))),
        ("aspect_ratio", args.aspect_ratio.as_ref().map(|v| json!(v))),
        ("size", args.size.as_ref().map(|v| json!(v))),
        ("seed", args.seed.as_ref().map(|n| json!(n))),
        ("generate_audio", args.generate_audio.then_some(json!(true))),
        ("action", args.action.as_ref().map(|v| json!(v))),
        ("edit", args.edit.as_ref().map(|v| json!(v))),
        (
            "input_video",
            args.input_video.as_deref().map(path_text).transpose()?,
        ),
    ] {
        if let Some(value) = value {
            request.insert(key.into(), value);
        }
    }
    if !args.params.is_empty() {
        request.insert("params".into(), Value::Object(params(&args.params)?));
    }
    if !args.references.is_empty() {
        let references = args
            .references
            .iter()
            .map(|reference| {
                let (kind, path) = reference.split_once(':').with_context(|| {
                    format!("--reference {reference} must be KIND:PATH, e.g. image:face.png")
                })?;
                Ok(json!({"kind": kind, "path": path}))
            })
            .collect::<Result<Vec<_>>>()?;
        request.insert("references".into(), Value::Array(references));
    }
    request.insert("output".into(), path_text(&args.output)?);
    Ok(Value::Object(request))
}

fn path_text(path: &Path) -> Result<Value> {
    path.to_str()
        .map(|text| json!(text))
        .with_context(|| format!("{} is not valid UTF-8", path.display()))
}

/// `KEY=VALUE` parameters: the value is JSON when it parses as JSON, else a
/// string; a key given twice is an error, not a silent override.
fn params(pairs: &[String]) -> Result<Map<String, Value>> {
    let mut params = Map::new();
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .filter(|(key, _)| !key.is_empty())
            .with_context(|| format!("--param {pair} must be KEY=VALUE"))?;
        let value = serde_json::from_str(value).unwrap_or_else(|_| json!(value));
        if params.insert(key.to_owned(), value).is_some() {
            bail!("--param {key} is given twice");
        }
    }
    Ok(params)
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

    #[test]
    fn params_are_json_when_they_parse_and_text_otherwise() {
        let read =
            |pairs: &[&str]| params(&pairs.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>());
        assert_eq!(
            Value::Object(
                read(&[
                    "steps=30",
                    "allow_community=true",
                    "adapter=/m/lora",
                    "cfg=1.5",
                    "n=\"7\""
                ])
                .unwrap()
            ),
            json!({"steps": 30, "allow_community": true, "adapter": "/m/lora", "cfg": 1.5, "n": "7"})
        );
        assert!(read(&["steps"]).is_err());
        assert!(read(&["=1"]).is_err());
        assert!(read(&["a=1", "a=2"])
            .unwrap_err()
            .to_string()
            .contains("twice"));
    }

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
