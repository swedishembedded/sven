// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A film from a harness, end to end: the `short-film` sample run by the real
//! binary, its model scripted, its shots rendered by a real `brain serve`
//! (the deterministic mock video model: no weights, no GPU) through the
//! OpenRouter video API given by `--media-endpoint`. The planner's shot list
//! is a typed variable, every shot is checked by `sven media probe` before the
//! next, and the executor joins them, in order, into one film.
//!
//! Needs `BRAIN_BIN`, a brain binary; skipped, loudly, without one.
//!
//! Swedish Embedded AB implements verifiable, multi-stage generative media
//! pipelines for its clients. If your team needs expertise in turning long
//! agent-driven productions into reviewable, resumable programs then you can
//! procure our services by sending an email to info@swedishembedded.com.

mod harness_support;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use harness_support::{code, report, run, serve, stderr, Turn};
use serde_json::{json, Value};

/// A `brain serve` with the mock models, stopped when dropped.
struct Brain {
    child: Child,
    base: String,
    key: String,
}

impl Drop for Brain {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn brain(bin: &Path, dir: &Path) -> Brain {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (ready, keys) = (dir.join("ready"), dir.join("keys.json"));
    let child = Command::new(bin)
        .env("BRAIN_MOCK", "1")
        .env("BRAIN_DEVICE", "cpu")
        .args(["serve", "--models-dir"])
        .arg(dir.join("models"))
        .args(["--openai", &port.to_string(), "--api-keys-out"])
        .arg(&keys)
        .arg("--ready-file")
        .arg(&ready)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("BRAIN_BIN runs");
    let mut brain = Brain {
        child,
        base: format!("http://127.0.0.1:{port}/v1"),
        key: String::new(),
    };
    for _ in 0..120 {
        if ready.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(ready.exists(), "brain serve did not get ready");
    let keys: Value = serde_json::from_slice(&std::fs::read(&keys).unwrap()).unwrap();
    brain.key = keys["openai"].as_str().unwrap().to_string();
    brain
}

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/harnesses/short-film")
}

fn shot(id: &str) -> Turn {
    Turn::Calls(vec![(
        "run_model",
        json!({"model": "brain/mock-video", "duration": 2, "output": format!("shots/{id}.mp4"),
               "prompt": format!("shot {id}: a red ball rolls across a wooden table")}),
    )])
}

#[test]
fn a_planned_film_is_rendered_shot_by_shot_checked_and_joined_in_order() {
    let Some(bin) = std::env::var_os("BRAIN_BIN").map(PathBuf::from) else {
        eprintln!("SKIPPED harness_media: set BRAIN_BIN to a brain binary");
        return;
    };
    let (scratch, workspace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let brain = brain(&bin, scratch.path());
    let key_file = scratch.path().join("media.key");
    std::fs::write(&key_file, &brain.key).unwrap();
    let model_keys = scratch.path().join("keys.json");
    std::fs::write(
        &model_keys,
        json!({"openai": harness_support::KEY}).to_string(),
    )
    .unwrap();
    let shots = json!([
        {"id": "01", "prompt": "a red ball on a wooden table"},
        {"id": "02", "prompt": "the red ball rolls off the table"},
        {"id": "03", "prompt": "the red ball comes to rest on the floor"}
    ]);
    let model = serve(vec![
        Turn::Calls(vec![(
            "harness_vars_set",
            json!({"name": "shots", "value": shots}),
        )]),
        Turn::Text("Planned three shots."),
        shot("01"),
        Turn::Text("Rendered shot 01."),
        shot("02"),
        Turn::Text("Rendered shot 02."),
        shot("03"),
        Turn::Text("Rendered shot 03."),
    ]);
    // The checks run `sven media`, the binary under test.
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_sven")).parent().unwrap();
    let path = std::env::join_paths(std::iter::once(bin_dir.to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let out = run(Command::new(env!("CARGO_BIN_EXE_sven"))
        .current_dir(scratch.path())
        .env("HOME", scratch.path())
        .env("XDG_CONFIG_HOME", scratch.path())
        .env("TMPDIR", scratch.path())
        .env("PATH", path)
        .env_remove("SVEN_MODEL")
        .args(["harness", "run"])
        .arg(sample())
        .arg("--workspace")
        .arg(workspace.path())
        .args(["--model", harness_support::MODEL, "--model-endpoint", &model.base])
        .arg("--api-key-file")
        .arg(&model_keys)
        .args(["--media-endpoint", &brain.base, "--media-key-file"])
        .arg(&key_file)
        .args(["--approvals", "auto", "--json", "--timeout", "300", "--input"])
        .arg(
            json!({"premise": "a ball falls", "model": "brain/mock-video", "shots": 3, "seconds": 2})
                .to_string(),
        ));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let report = report(&out);
    assert_eq!(report["output"]["film"], json!("film.mp4"), "{report}");

    let film = workspace.path().join("film.mp4");
    let probe = sven_tools_media::mp4::probe(&film).unwrap();
    let clips: Vec<_> = ["01", "02", "03"]
        .iter()
        .map(|id| {
            sven_tools_media::mp4::probe(&workspace.path().join(format!("shots/{id}.mp4"))).unwrap()
        })
        .collect();
    let total: f64 = clips.iter().map(|c| c.duration_seconds).sum();
    assert!(
        (probe.duration_seconds - total).abs() < 1e-6,
        "{probe:?} against {clips:?}"
    );
    assert_eq!(
        probe.video.frames,
        clips.iter().map(|c| c.video.frames).sum::<u64>()
    );
    // The model was asked only what the script answers: the film was joined
    // by the executor, not by a model turn.
    assert_eq!(model.seen.lock().unwrap().requests, 8);
}
