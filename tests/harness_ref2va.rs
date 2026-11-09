// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `ref2va` sample, run by the real binary with no model at all: its
//! render is `sven media generate` against the fake video server the media
//! tool's tests use, its result is checked by `sven media probe`. The specs:
//! the request carries the reference, the action and exactly the parameters
//! the inputs name; running it again submits nothing; a refused licence or a
//! reference that is no image stops it before any request; a clip of the
//! wrong size fails the run at the probe.
//!
//! Swedish Embedded AB implements reproducible, verifiable generative-video
//! pipelines for its clients. If your team needs expertise in running video
//! models from scripts and CI then you can procure our services by sending an
//! email to info@swedishembedded.com.

mod harness_support;

use std::path::Path;
use std::process::{Command, Output};

use harness_support::cli::{code, run, sample, stderr, stdout_json};
use serde_json::{json, Value};
use sven_tools_media::fake::{rendered_clip, serve_in_background, Fake};
use sven_tools_media::mp4::fixture::Clip;

/// A scratch home whose configuration names the fake server as the media
/// provider, and a workspace holding one reference image.
struct Run {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
}

impl Run {
    fn new(fake: &Fake) -> Self {
        let home = tempfile::tempdir().unwrap();
        // XDG_CONFIG_HOME is the home directory itself in this run.
        std::fs::create_dir(home.path().join("sven")).unwrap();
        std::fs::write(
            home.path().join("sven/config.yaml"),
            format!(
                "tools:\n  media:\n    provider: video\nproviders:\n  video:\n    base_url: {}\n    api_key: k\n",
                fake.base_url
            ),
        )
        .unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("face.png"), grey_png(8, 8)).unwrap();
        Self { home, workspace }
    }

    /// `sven harness run` of the sample with `inputs` over the usual ones: no
    /// `--model`, because no turn is taken.
    fn sven(&self, inputs: Value) -> Output {
        let mut all = json!({
            "prompt": "<Picture 1> turns and waves", "reference": "face.png",
            "model": "brain/mock-video", "width": 64, "height": 48, "frames": 9,
            "seconds": 0.375, "steps": 4, "seed": 3, "preset": "base",
            "licence_confirmed": true, "output": "result.mp4",
        });
        all.as_object_mut()
            .unwrap()
            .extend(inputs.as_object().unwrap().clone());
        // The gates run `sven`, the binary under test.
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_sven")).parent().unwrap();
        let path = std::env::join_paths(std::iter::once(bin_dir.to_path_buf()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        run(Command::new(env!("CARGO_BIN_EXE_sven"))
            .current_dir(self.home.path())
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path())
            .env("TMPDIR", self.home.path())
            .env("PATH", path)
            .env_remove("SVEN_MODEL")
            .args(["harness", "run"])
            .arg(sample("ref2va"))
            .arg("--workspace")
            .arg(self.workspace.path())
            .args(["--approvals", "auto", "--json", "--trace"])
            .arg(self.home.path().join("trace.jsonl"))
            .arg("--input")
            .arg(all.to_string()))
    }
}

/// A grey PNG, made here: signature, header, one stored-deflate data chunk, end.
fn grey_png(width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend(kind);
        out.extend(data);
        out.extend(crc32(&[kind.as_slice(), data].concat()).to_be_bytes());
        out
    }
    let row_bytes = 1 + width as usize;
    let raw: Vec<u8> = (0..height as usize * row_bytes)
        .map(|i| if i % row_bytes == 0 { 0 } else { 0x80 })
        .collect();
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend((raw.len() as u16).to_le_bytes());
    zlib.extend((!(raw.len() as u16)).to_le_bytes());
    zlib.extend(&raw);
    zlib.extend(((b << 16) | a).to_be_bytes());
    let header = [
        width.to_be_bytes().as_slice(),
        &height.to_be_bytes(),
        &[8, 0, 0, 0, 0],
    ]
    .concat();
    [
        b"\x89PNG\r\n\x1a\n".to_vec(),
        chunk(b"IHDR", &header),
        chunk(b"IDAT", &zlib),
        chunk(b"IEND", &[]),
    ]
    .concat()
}

fn bodies(fake: &Fake) -> Vec<Value> {
    fake.created
        .lock()
        .unwrap()
        .iter()
        .map(|(_, body)| body.clone())
        .collect()
}

#[test]
fn the_clip_is_rendered_from_the_reference_checked_exactly_and_published_with_no_model() {
    let fake = serve_in_background(0, rendered_clip);
    let run = Run::new(&fake);
    // The base preset: the release alone.
    let out = run.sven(json!({}));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let report = stdout_json(&out);
    assert_eq!(report["status"], "completed", "{report}");
    assert_eq!(report["output"]["clip"], "result.mp4");

    let clip = sven_tools_media::mp4::probe(&run.workspace.path().join("result.mp4")).unwrap();
    assert_eq!(
        (clip.video.width, clip.video.height, clip.video.frames),
        (64, 48, 9)
    );
    assert!(clip.audio.is_some());

    let sent = bodies(&fake);
    assert_eq!(sent.len(), 1);
    let body = &sent[0];
    assert_eq!(body["model"], "brain/mock-video");
    assert_eq!(body["size"], "64x48");
    assert_eq!(body["seed"], 3);
    assert!(
        body.get("duration").is_none(),
        "the frame count is the length"
    );
    assert_eq!(body["input_references"][0]["type"], "image_url");
    assert_eq!(
        body["provider"]["options"]["brain"],
        json!({"action": "ref2va", "params": {"steps": 4, "num_frames": 9, "allow_community": true, "preset": "base"}})
    );

    // The trace records the clip's digest where a host can read it.
    let trace = std::fs::read_to_string(run.home.path().join("trace.jsonl")).unwrap();
    let published: Value = trace
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| record["event"] == "published")
        .expect("the clip is published");
    let file = &published["files"][0];
    assert_eq!(file["path"], "result.mp4");
    let bytes = std::fs::read(run.workspace.path().join("result.mp4")).unwrap();
    assert_eq!(
        file["digest"].as_str().unwrap(),
        published_digest("result.mp4", &bytes),
        "the digest binds the path to the clip's bytes"
    );
}

/// The digest a publication records for one file: the SHA-256 of the path, a
/// zero byte, the hex SHA-256 of the bytes and a newline.
fn published_digest(path: &str, bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let content = hex::encode(Sha256::digest(bytes));
    hex::encode(Sha256::digest(format!("{path}\0{content}\n")))
}

#[test]
fn running_it_again_submits_nothing_and_another_preset_is_another_request() {
    let fake = serve_in_background(0, rendered_clip);
    let run = Run::new(&fake);
    assert_eq!(code(&run.sven(json!({}))), 0);
    let again = run.sven(json!({}));
    assert_eq!(code(&again), 0, "{}", stderr(&again));
    assert_eq!(bodies(&fake).len(), 1);
    assert_eq!(
        bodies(&fake)[0]["provider"]["options"]["brain"]["params"]["preset"],
        "base"
    );

    // A preset names the server's own adapter and gates: the request never
    // carries a path on the server.
    let fast = run.sven(json!({"preset": "turbo-4step-vsa"}));
    assert_eq!(code(&fast), 0, "{}", stderr(&fast));
    let sent = bodies(&fake);
    assert_eq!(sent.len(), 2);
    let params = &sent[1]["provider"]["options"]["brain"]["params"];
    assert_eq!(params["preset"], "turbo-4step-vsa");
    assert!(params.get("adapter").is_none(), "{params}");
}

#[test]
fn an_unconfirmed_licence_or_a_reference_that_is_no_image_stops_before_any_request() {
    let fake = serve_in_background(0, rendered_clip);
    let run = Run::new(&fake);
    std::fs::write(run.workspace.path().join("notes.txt"), "text").unwrap();
    for (inputs, reason) in [
        (json!({"licence_confirmed": false}), "licence"),
        (json!({"reference": "notes.txt"}), "png, jpg, jpeg or webp"),
        (json!({"reference": "missing.png"}), "missing.png"),
        (json!({"reference": "../face.png"}), "inside the workspace"),
        (json!({"output": "/abs/result.mp4"}), "inside the workspace"),
    ] {
        let out = run.sven(inputs.clone());
        assert_eq!(code(&out), 1, "{inputs}: {}", stderr(&out));
        assert!(stderr(&out).contains(reason), "{inputs}: {}", stderr(&out));
    }
    assert!(bodies(&fake).is_empty());
    assert!(!run.workspace.path().join("result.mp4").exists());
}

#[test]
fn a_clip_of_the_wrong_size_fails_the_run_at_the_probe() {
    fn small(_: &Value) -> Vec<u8> {
        Clip::new(0, 9).with_size(32, 32).with_sound(1).to_bytes()
    }
    let fake = serve_in_background(0, small);
    let run = Run::new(&fake);
    let out = run.sven(json!({}));
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("is 32 px wide, not 64"),
        "{}",
        stderr(&out)
    );
}
