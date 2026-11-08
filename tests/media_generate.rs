// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven media generate`: the real binary against the fake video server the
//! media tool's own tests use. The request it sends is the one `run_model`
//! sends for the same arguments, the same request twice submits once, and a
//! reference it cannot vouch for is refused before the network is touched.
//!
//! Swedish Embedded AB implements reproducible, command-line driven generative
//! video pipelines for its clients. If your team needs expertise in running
//! video models from scripts and CI then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{json, Value};
use sven_tools_media::fake::{rendered_clip, serve_in_background, Fake};
use sven_tools_media::{generate, MediaConfig, MediaEndpoint};

/// A working directory with a configuration naming `fake` as the media
/// provider (`providers.video`) and one reference image.
struct Workspace {
    dir: tempfile::TempDir,
}

impl Workspace {
    fn new(base_url: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.yaml"),
            format!(
                "tools:\n  media:\n    provider: video\nproviders:\n  video:\n    base_url: {base_url}\n    api_key: k\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.path().join("face.png"), b"png-bytes").unwrap();
        Self { dir }
    }

    fn sven(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sven"))
            .current_dir(self.dir.path())
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path())
            .env_remove("SVEN_MODEL")
            .arg("--config")
            .arg(self.dir.path().join("config.yaml"))
            .args(["media", "generate"])
            .args(args)
            .output()
            .unwrap()
    }
}

/// The flags of one ref2va request, and the arguments `run_model` takes for it.
const FLAGS: [&str; 22] = [
    "--model",
    "brain/mock-video",
    "--prompt",
    "<Picture 1> waves",
    "--action",
    "ref2va",
    "--size",
    "64x48",
    "--seed",
    "7",
    "--param",
    "num_frames=9",
    "--param",
    "allow_community=true",
    "--param",
    "adapter=/models/lora",
    "--reference",
    "image:face.png",
    "--duration",
    "5",
    "--output",
    "out/shot.mp4",
];

fn run_model_args() -> Value {
    json!({
        "model": "brain/mock-video", "prompt": "<Picture 1> waves", "action": "ref2va",
        "size": "64x48", "seed": 7, "duration": 5, "output": "out/shot.mp4",
        "params": {"num_frames": 9, "allow_community": true, "adapter": "/models/lora"},
        "references": [{"path": "face.png", "kind": "image"}],
    })
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn submissions(fake: &Fake) -> usize {
    fake.created.lock().unwrap().len()
}

#[test]
fn the_body_sent_is_the_one_run_model_sends_for_the_same_arguments() {
    let (by_command, by_tool) = (
        serve_in_background(0, rendered_clip),
        serve_in_background(0, rendered_clip),
    );
    let workspace = Workspace::new(&by_command.base_url);
    let out = workspace.sven(&FLAGS);
    assert!(out.status.success(), "{}", stderr(&out));

    let tool_dir = Workspace::new(&by_tool.base_url);
    let endpoint = MediaEndpoint {
        base_url: by_tool.base_url.clone(),
        api_key: "k".into(),
    };
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(generate(
            &endpoint,
            &MediaConfig::default(),
            tool_dir.dir.path(),
            &run_model_args(),
            &mut |_| {},
        ))
        .unwrap();

    let sent = by_command.created.lock().unwrap();
    let expected = by_tool.created.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, expected[0].1);
    // Not just equal to itself: the params and the reference are in it.
    assert_eq!(
        sent[0].1["provider"]["options"]["brain"]["params"],
        json!({"num_frames": 9, "allow_community": true, "adapter": "/models/lora"})
    );
    assert_eq!(sent[0].1["input_references"][0]["type"], "image_url");
    assert_eq!(sent[0].1["duration"], json!(5));
}

#[test]
fn the_written_path_is_printed_and_json_describes_the_clip() {
    let fake = serve_in_background(0, rendered_clip);
    let workspace = Workspace::new(&fake.base_url);
    let out = workspace.sven(&FLAGS);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "out/shot.mp4");
    assert!(stderr(&out).contains("job0"), "{}", stderr(&out));

    let mut flags = FLAGS.to_vec();
    flags.push("--json");
    let again = workspace.sven(&flags);
    assert!(again.status.success(), "{}", stderr(&again));
    let described: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(described["path"], "out/shot.mp4");
    assert_eq!(
        (
            described["width"].as_u64(),
            described["height"].as_u64(),
            described["frames"].as_u64()
        ),
        (Some(64), Some(48), Some(9))
    );
    assert_eq!(described["digest"].as_str().unwrap().len(), 64);
    assert!(described["seconds"].as_f64().unwrap() > 0.0);
    assert!(Path::new(&workspace.dir.path().join("out/shot.mp4")).is_file());
}

#[test]
fn the_same_request_twice_submits_once() {
    let fake = serve_in_background(0, rendered_clip);
    let workspace = Workspace::new(&fake.base_url);
    assert!(workspace.sven(&FLAGS).status.success());
    // Asked for somewhere else, still one render.
    let mut flags = FLAGS.to_vec();
    *flags.last_mut().unwrap() = "again.mp4";
    let again = workspace.sven(&flags);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(
        stderr(&again).contains("already made"),
        "{}",
        stderr(&again)
    );
    assert_eq!(submissions(&fake), 1);
    assert_eq!(
        std::fs::read(workspace.dir.path().join("again.mp4")).unwrap(),
        std::fs::read(workspace.dir.path().join("out/shot.mp4")).unwrap()
    );
    // Another seed is another take.
    let mut flags = FLAGS.to_vec();
    flags[9] = "8";
    assert!(workspace.sven(&flags).status.success());
    assert_eq!(submissions(&fake), 2);
}

#[test]
fn a_reference_that_cannot_be_vouched_for_is_refused_before_any_request() {
    let fake = serve_in_background(0, rendered_clip);
    let workspace = Workspace::new(&fake.base_url);
    let refused = |reference: &str, wanted: &str| {
        let out = workspace.sven(&[
            "--model",
            "m",
            "--prompt",
            "x",
            "--reference",
            reference,
            "--output",
            "o.mp4",
        ]);
        assert!(!out.status.success(), "{reference}");
        assert!(
            stderr(&out).contains(wanted),
            "{reference}: {}",
            stderr(&out)
        );
    };
    refused("sprite:face.png", "sprite");
    refused("face.png", "KIND:PATH");
    refused("image:missing.png", "missing.png");
    refused("image:/etc/hostname", "working directory");
    refused("audio:face.png", "audio");
    assert_eq!(submissions(&fake), 0);
    assert!(!workspace.dir.path().join("o.mp4").exists());
}

#[test]
fn a_provider_that_refuses_fails_the_command_with_its_reason() {
    let fake = serve_in_background(3, rendered_clip);
    let workspace = Workspace::new(&fake.base_url);
    let out = workspace.sven(&[
        "--model",
        "brain/mock-video",
        "--prompt",
        "x",
        "--output",
        "o.mp4",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("the model ran out of memory"),
        "{}",
        stderr(&out)
    );
    assert!(!workspace.dir.path().join("o.mp4").exists());
}
