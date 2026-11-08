// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use super::read::{Edit, Kind, Movie, RATE_ONE};
use super::*;
pub(crate) use fixture::Clip;

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
    Clip::new(1, 24).with_size(128, 96).write(&b);
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
