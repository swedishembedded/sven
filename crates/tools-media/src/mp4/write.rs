// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The joined file: each track's samples from every clip in order, the
//! edits that play them, and the file laid out with its index first.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result};

use super::boxes::{atoms, boxed, child, full_box, Reader};
use super::read::{rescale, Edit, Kind, Movie, Sample, RATE_ONE};

/// The longest stretch of one track written as one chunk, in seconds.
const CHUNK_SECONDS: u64 = 1;

/// One track of the joined file: which clip each sample comes from, and the
/// edits that play it.
pub(super) struct Joined {
    pub(super) kind: Kind,
    /// (clip, sample index in that clip's track)
    pub(super) samples: Vec<(usize, usize)>,
    pub(super) edits: Vec<Edit>,
}

impl Joined {
    pub(super) fn new(
        movies: &[Movie],
        lengths: &[u64],
        timescale: u32,
        kind: Kind,
    ) -> Result<Self> {
        let mut samples = Vec::new();
        let mut edits: Vec<Edit> = Vec::new();
        let mut media_start = 0i64;
        for (clip, (movie, &length)) in movies.iter().zip(lengths).enumerate() {
            let track = movie.track(kind)?;
            samples.extend((0..track.samples.len()).map(|i| (clip, i)));
            // The clip's own edits, moved to where its samples now start and
            // cut or padded to the length of its picture.
            let mut left = length;
            for edit in track.plays(timescale, movie.timescale) {
                if left == 0 {
                    break;
                }
                let duration = edit.duration.min(left);
                left -= duration;
                push_edit(
                    &mut edits,
                    Edit {
                        duration,
                        media_time: if edit.media_time < 0 {
                            -1
                        } else {
                            edit.media_time + media_start
                        },
                        rate: edit.rate,
                    },
                    track.timescale,
                    timescale,
                );
            }
            if left > 0 {
                push_edit(
                    &mut edits,
                    Edit {
                        duration: left,
                        media_time: -1,
                        rate: RATE_ONE,
                    },
                    track.timescale,
                    timescale,
                );
            }
            media_start += i64::try_from(track.media_duration())?;
        }
        Ok(Self {
            kind,
            samples,
            edits,
        })
    }

    /// The edit list to write, or none when the track simply plays from its
    /// start to its end.
    pub(super) fn edit_list(&self, movies: &[Movie]) -> Option<&[Edit]> {
        let media: u64 = self
            .samples
            .iter()
            .map(|&(clip, i)| u64::from(sample_of(movies, self.kind, clip, i).delta))
            .sum();
        let trivial = match self.edits.as_slice() {
            [only] => {
                let first = movies[0].track(self.kind).ok();
                only.media_time == 0
                    && only.rate == RATE_ONE
                    && first.is_some_and(|t| {
                        rescale(media, t.timescale, movies[0].timescale) == only.duration
                    })
            }
            _ => false,
        };
        (!trivial).then_some(self.edits.as_slice())
    }
}

/// Adds `edit`, merged into the last one when it simply carries on from it.
pub(super) fn push_edit(
    edits: &mut Vec<Edit>,
    edit: Edit,
    track_timescale: u32,
    movie_timescale: u32,
) {
    if edit.duration == 0 {
        return;
    }
    if let Some(last) = edits.last_mut() {
        let continues = if last.media_time < 0 {
            edit.media_time < 0
        } else {
            edit.media_time >= 0
                && last.rate == RATE_ONE
                && edit.rate == RATE_ONE
                && last.media_time
                    + i64::try_from(rescale(last.duration, movie_timescale, track_timescale))
                        .unwrap_or(i64::MAX)
                    == edit.media_time
        };
        if continues {
            last.duration += edit.duration;
            return;
        }
    }
    edits.push(edit);
}

pub(super) fn sample_of(movies: &[Movie], kind: Kind, clip: usize, index: usize) -> Sample {
    let track = match kind {
        Kind::Video => movies[clip].video().ok(),
        Kind::Audio => movies[clip].audio(),
    };
    track
        .map(|t| t.samples[index])
        .expect("joined tracks name samples their clips have")
}

/// A run of samples of one track written together.
pub(super) struct Chunk {
    pub(super) track: usize,
    /// Index range into that track's `samples`.
    pub(super) first: usize,
    pub(super) count: usize,
    /// (clip, decode time in seconds) of the first sample, the order chunks
    /// are written in.
    pub(super) starts: (usize, f64),
}

/// Chunks of at most a second each, never across clips, interleaved by time.
pub(super) fn chunks(movies: &[Movie], tracks: &[Joined]) -> Vec<Chunk> {
    let mut out = Vec::new();
    for (t, track) in tracks.iter().enumerate() {
        let mut index = 0;
        // Decode time within the current clip, in that clip's track timescale.
        let (mut clip_of_time, mut time) = (usize::MAX, 0u64);
        while index < track.samples.len() {
            let (clip, _) = track.samples[index];
            if clip != clip_of_time {
                (clip_of_time, time) = (clip, 0);
            }
            let timescale = movies[clip].track(track.kind).map_or(1, |t| t.timescale);
            let start = time;
            let mut count = 0;
            while index + count < track.samples.len() {
                let (c, i) = track.samples[index + count];
                if c != clip || (count > 0 && time - start >= CHUNK_SECONDS * u64::from(timescale))
                {
                    break;
                }
                time += u64::from(sample_of(movies, track.kind, c, i).delta);
                count += 1;
            }
            out.push(Chunk {
                track: t,
                first: index,
                count,
                starts: (clip, start as f64 / f64::from(timescale)),
            });
            index += count;
        }
    }
    out.sort_by(|a, b| {
        a.starts
            .0
            .cmp(&b.starts.0)
            .then(a.starts.1.total_cmp(&b.starts.1))
            .then(a.track.cmp(&b.track))
    });
    out
}

/// Run-length entries of `values`.
pub(super) fn runs<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for value in values {
        match out.last_mut() {
            Some((n, last)) if *last == value => *n += 1,
            _ => out.push((1, value)),
        }
    }
    out
}

pub(super) fn stbl(
    movies: &[Movie],
    track: &Joined,
    chunks: &[Chunk],
    t: usize,
    offsets: &[u64],
    wide: bool,
) -> Vec<u8> {
    let first = movies[0]
        .track(track.kind)
        .expect("the first clip has the joined track");
    let samples: Vec<Sample> = track
        .samples
        .iter()
        .map(|&(clip, i)| sample_of(movies, track.kind, clip, i))
        .collect();
    let mut out = boxed(b"stsd", &first.stsd);

    let stts = runs(samples.iter().map(|s| s.delta));
    let mut payload = (stts.len() as u32).to_be_bytes().to_vec();
    for (n, delta) in stts {
        payload.extend(n.to_be_bytes());
        payload.extend(delta.to_be_bytes());
    }
    out.extend(full_box(b"stts", 0, 0, &payload));

    let with_composition = track.samples.iter().any(|&(clip, _)| {
        movies[clip]
            .track(track.kind)
            .is_ok_and(|t| t.has_composition)
    });
    if with_composition {
        let ctts = runs(samples.iter().map(|s| s.composition));
        let negative = ctts.iter().any(|&(_, o)| o < 0);
        let mut payload = (ctts.len() as u32).to_be_bytes().to_vec();
        for (n, offset) in ctts {
            payload.extend(n.to_be_bytes());
            payload.extend((offset as i32).to_be_bytes());
        }
        out.extend(full_box(b"ctts", u8::from(negative), 0, &payload));
    }

    let with_sync_table = track.samples.iter().any(|&(clip, _)| {
        movies[clip]
            .track(track.kind)
            .is_ok_and(|t| t.has_sync_table)
    });
    if with_sync_table {
        let keys: Vec<u32> = samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.sync)
            .map(|(i, _)| i as u32 + 1)
            .collect();
        let mut payload = (keys.len() as u32).to_be_bytes().to_vec();
        for key in keys {
            payload.extend(key.to_be_bytes());
        }
        out.extend(full_box(b"stss", 0, 0, &payload));
    }

    let mine: Vec<&Chunk> = chunks.iter().filter(|c| c.track == t).collect();
    let mut stsc = Vec::new();
    for (number, chunk) in mine.iter().enumerate() {
        if stsc
            .last()
            .is_none_or(|&(_, per): &(u32, u32)| per != chunk.count as u32)
        {
            stsc.push((number as u32 + 1, chunk.count as u32));
        }
    }
    let mut payload = (stsc.len() as u32).to_be_bytes().to_vec();
    for (first_chunk, per) in stsc {
        payload.extend(first_chunk.to_be_bytes());
        payload.extend(per.to_be_bytes());
        payload.extend(1u32.to_be_bytes());
    }
    out.extend(full_box(b"stsc", 0, 0, &payload));

    let mut payload = 0u32.to_be_bytes().to_vec();
    payload.extend((samples.len() as u32).to_be_bytes());
    for sample in &samples {
        payload.extend(sample.size.to_be_bytes());
    }
    out.extend(full_box(b"stsz", 0, 0, &payload));

    let mut payload = (offsets.len() as u32).to_be_bytes().to_vec();
    for &offset in offsets {
        if wide {
            payload.extend(offset.to_be_bytes());
        } else {
            payload.extend((offset as u32).to_be_bytes());
        }
    }
    out.extend(full_box(
        if wide { b"co64" } else { b"stco" },
        0,
        0,
        &payload,
    ));
    out
}

/// `mvhd`, `tkhd` or `mdhd` rewritten as version 1 with `duration` (and the
/// track id, for `tkhd`); every other field is kept.
pub(super) fn with_duration(
    header: &[u8],
    tkhd_id: Option<u32>,
    duration: u64,
) -> Result<(u32, Vec<u8>)> {
    let mut r = Reader::new(header);
    let version = r.u8()?;
    let flags = r.take(3)?;
    let (created, modified) = if version == 1 {
        (r.u64()?, r.u64()?)
    } else {
        (u64::from(r.u32()?), u64::from(r.u32()?))
    };
    let middle = r.u32()?; // timescale (mvhd, mdhd) or track id (tkhd)
    let mut out = vec![1u8];
    out.extend(flags);
    out.extend(created.to_be_bytes());
    out.extend(modified.to_be_bytes());
    match tkhd_id {
        Some(id) => {
            out.extend(id.to_be_bytes());
            out.extend(r.take(4)?); // reserved
        }
        None => out.extend(middle.to_be_bytes()),
    }
    if version == 1 {
        r.u64()?;
    } else {
        r.u32()?;
    }
    out.extend(duration.to_be_bytes());
    out.extend(&header[r.at..]);
    Ok((middle, out))
}

pub(super) fn trak(movies: &[Movie], track: &Joined, id: u32, stbl: Vec<u8>) -> Result<Vec<u8>> {
    let first = movies[0].track(track.kind)?;
    let presented: u64 = track.edits.iter().map(|e| e.duration).sum();
    let media: u64 = track
        .samples
        .iter()
        .map(|&(clip, i)| u64::from(sample_of(movies, track.kind, clip, i).delta))
        .sum();
    let mut out = Vec::new();
    let (_, tkhd) = with_duration(child(&first.trak, b"tkhd")?, Some(id), presented)?;
    out.extend(boxed(b"tkhd", &tkhd));
    if let Some(edits) = track.edit_list(movies) {
        let mut payload = (edits.len() as u32).to_be_bytes().to_vec();
        for edit in edits {
            payload.extend(edit.duration.to_be_bytes());
            payload.extend(edit.media_time.to_be_bytes());
            payload.extend(edit.rate.to_be_bytes());
        }
        out.extend(boxed(b"edts", &full_box(b"elst", 1, 0, &payload)));
    }
    let mdia_in = child(&first.trak, b"mdia")?;
    let mut mdia = Vec::new();
    for atom in atoms(mdia_in)? {
        match &atom.kind {
            b"mdhd" => mdia.extend(boxed(b"mdhd", &with_duration(atom.payload, None, media)?.1)),
            b"minf" => {
                let mut minf = Vec::new();
                for inner in atoms(atom.payload)? {
                    if &inner.kind == b"stbl" {
                        minf.extend(boxed(b"stbl", &stbl));
                    } else {
                        minf.extend(boxed(&inner.kind, inner.payload));
                    }
                }
                mdia.extend(boxed(b"minf", &minf));
            }
            kind => mdia.extend(boxed(kind, atom.payload)),
        }
    }
    out.extend(boxed(b"mdia", &mdia));
    Ok(boxed(b"trak", &out))
}

pub(super) fn moov(
    movies: &[Movie],
    first: &Movie,
    tracks: &[Joined],
    chunks: &[Chunk],
    base: u64,
    wide: bool,
) -> Result<Vec<u8>> {
    let mut at = base;
    let mut offsets = vec![Vec::new(); tracks.len()];
    for chunk in chunks {
        offsets[chunk.track].push(at);
        let track = &tracks[chunk.track];
        at += track.samples[chunk.first..chunk.first + chunk.count]
            .iter()
            .map(|&(clip, i)| u64::from(sample_of(movies, track.kind, clip, i).size))
            .sum::<u64>();
    }
    let presented = tracks[0].edits.iter().map(|e| e.duration).sum();
    let (_, mut mvhd) = with_duration(&first.mvhd, None, presented)?;
    let next_track = mvhd.len() - 4;
    mvhd[next_track..].copy_from_slice(&(tracks.len() as u32 + 1).to_be_bytes());
    let mut out = boxed(b"mvhd", &mvhd);
    for (t, track) in tracks.iter().enumerate() {
        let table = stbl(movies, track, chunks, t, &offsets[t], wide);
        out.extend(trak(movies, track, t as u32 + 1, table)?);
    }
    Ok(boxed(b"moov", &out))
}

pub(super) fn write_joined(
    movies: &[Movie],
    first: &Movie,
    tracks: &[Joined],
    output: &Path,
) -> Result<()> {
    let chunks = chunks(movies, tracks);
    let mdat_payload: u64 = tracks
        .iter()
        .flat_map(|track| {
            track
                .samples
                .iter()
                .map(|&(clip, i)| u64::from(sample_of(movies, track.kind, clip, i).size))
        })
        .sum();
    let mdat_header: u64 = if mdat_payload + 8 > u64::from(u32::MAX) {
        16
    } else {
        8
    };
    // The table is the same size whatever offsets it holds, so lay it out once
    // to learn where the samples will start.
    let probe_wide = mdat_payload > u64::from(u32::MAX) / 2;
    let sized = moov(movies, first, tracks, &chunks, 0, probe_wide)?;
    let base = first.ftyp.len() as u64 + sized.len() as u64 + mdat_header;
    let wide = base + mdat_payload > u64::from(u32::MAX);
    let sized = if wide == probe_wide {
        sized
    } else {
        moov(movies, first, tracks, &chunks, 0, wide)?
    };
    let base = first.ftyp.len() as u64 + sized.len() as u64 + mdat_header;
    let moov = moov(movies, first, tracks, &chunks, base, wide)?;

    let dir = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = dir.join(format!(
        ".{}.part",
        output
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("joined.mp4")
    ));
    let written = (|| -> Result<()> {
        let mut out = BufWriter::new(File::create(&temporary)?);
        out.write_all(&first.ftyp)?;
        out.write_all(&moov)?;
        if mdat_header == 16 {
            out.write_all(&1u32.to_be_bytes())?;
            out.write_all(b"mdat")?;
            out.write_all(&(mdat_payload + 16).to_be_bytes())?;
        } else {
            out.write_all(&(mdat_payload as u32 + 8).to_be_bytes())?;
            out.write_all(b"mdat")?;
        }
        let mut sources: Vec<Option<File>> = movies.iter().map(|_| None).collect();
        let mut buf = Vec::new();
        for chunk in &chunks {
            let track = &tracks[chunk.track];
            for &(clip, i) in &track.samples[chunk.first..chunk.first + chunk.count] {
                let sample = sample_of(movies, track.kind, clip, i);
                let file = match &mut sources[clip] {
                    Some(file) => file,
                    slot => slot.insert(File::open(&movies[clip].path)?),
                };
                buf.resize(sample.size as usize, 0);
                file.seek(SeekFrom::Start(sample.offset))?;
                file.read_exact(&mut buf)?;
                out.write_all(&buf)?;
            }
        }
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    })();
    match written {
        Ok(()) => std::fs::rename(&temporary, output)
            .with_context(|| format!("writing {}", output.display())),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error.context(format!("writing {}", output.display())))
        }
    }
}
