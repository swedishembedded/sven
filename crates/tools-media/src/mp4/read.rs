// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A clip as it is laid out on disk: its tracks, their samples, timing and
//! edits, every sample checked to lie inside the file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, ensure, Context, Result};

use super::boxes::{atoms, boxed, child, fourcc, Reader, MAX_SAMPLES};

/// The largest `moov` read: its sample tables are held in memory.
const MAX_MOOV_BYTES: u64 = 64 * 1024 * 1024;
/// Playback at normal speed, as an edit list writes it (16.16 fixed point).
pub(super) const RATE_ONE: i32 = 0x0001_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Sample {
    pub(super) offset: u64,
    pub(super) size: u32,
    pub(super) delta: u32,
    pub(super) composition: i64,
    pub(super) sync: bool,
}

/// One edit: `duration` in the movie's timescale, `media_time` in the
/// track's (-1 for an empty edit, which shows nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Edit {
    pub(super) duration: u64,
    pub(super) media_time: i64,
    pub(super) rate: i32,
}

pub(super) struct Track {
    pub(super) kind: Kind,
    /// The whole `trak` payload, the source of the boxes copied as they are.
    pub(super) trak: Vec<u8>,
    pub(super) timescale: u32,
    /// The `stsd` box payload: the codec and its configuration.
    pub(super) stsd: Vec<u8>,
    pub(super) samples: Vec<Sample>,
    pub(super) has_composition: bool,
    pub(super) has_sync_table: bool,
    pub(super) edits: Option<Vec<Edit>>,
    pub(super) size: (u32, u32),
}

impl Track {
    pub(super) fn codec(&self) -> String {
        // stsd: version+flags, entry count, then the first entry's size and type.
        self.stsd.get(12..16).map(fourcc).unwrap_or_default()
    }

    pub(super) fn media_duration(&self) -> u64 {
        self.samples.iter().map(|s| u64::from(s.delta)).sum()
    }

    /// The edits this track plays, in a movie of `movie_timescale`: its own
    /// edit list, or all of its media from the start.
    pub(super) fn plays(&self, movie_timescale: u32, own_timescale: u32) -> Vec<Edit> {
        match &self.edits {
            Some(edits) if !edits.is_empty() => edits
                .iter()
                .map(|e| Edit {
                    duration: rescale(e.duration, own_timescale, movie_timescale),
                    ..*e
                })
                .collect(),
            _ => vec![Edit {
                duration: rescale(self.media_duration(), self.timescale, movie_timescale),
                media_time: 0,
                rate: RATE_ONE,
            }],
        }
    }

    /// How long the track plays in a movie of `movie_timescale`, read from a
    /// clip whose own movie timescale is `own_timescale`.
    pub(super) fn presented(&self, movie_timescale: u32, own_timescale: u32) -> u64 {
        self.plays(movie_timescale, own_timescale)
            .iter()
            .map(|e| e.duration)
            .sum()
    }

    pub(super) fn read(trak: &[u8], file_len: u64) -> Result<Option<Self>> {
        let mdia = child(trak, b"mdia")?;
        let hdlr = child(mdia, b"hdlr")?;
        let kind = match hdlr.get(8..12) {
            Some(b"vide") => Kind::Video,
            Some(b"soun") => Kind::Audio,
            _ => return Ok(None),
        };
        let mdhd = child(mdia, b"mdhd")?;
        let mut r = Reader::new(mdhd);
        let timescale = if r.version()? == 1 {
            r.take(16)?;
            r.u32()?
        } else {
            r.take(8)?;
            r.u32()?
        };
        ensure!(timescale > 0, "a track has no timescale");
        let tkhd = child(trak, b"tkhd")?;
        let size = tkhd
            .len()
            .checked_sub(8)
            .and_then(|at| tkhd.get(at..))
            .map(|wh| {
                (
                    u32::from_be_bytes([wh[0], wh[1], wh[2], wh[3]]) >> 16,
                    u32::from_be_bytes([wh[4], wh[5], wh[6], wh[7]]) >> 16,
                )
            })
            .unwrap_or((0, 0));
        let stbl = child(child(mdia, b"minf")?, b"stbl")?;
        let stsd = child(stbl, b"stsd")?.to_vec();
        ensure!(
            stsd.get(4..8) == Some(&1u32.to_be_bytes()[..]),
            "a track with several sample descriptions cannot be joined"
        );
        let samples = sample_table(stbl, file_len)?;
        let edits = match atoms(trak)?.iter().find(|a| &a.kind == b"edts") {
            Some(edts) => Some(edit_list(child(edts.payload, b"elst")?)?),
            None => None,
        };
        Ok(Some(Self {
            kind,
            trak: trak.to_vec(),
            timescale,
            stsd,
            has_composition: atoms(stbl)?.iter().any(|a| &a.kind == b"ctts"),
            has_sync_table: atoms(stbl)?.iter().any(|a| &a.kind == b"stss"),
            samples,
            edits,
            size,
        }))
    }
}

pub(super) fn edit_list(elst: &[u8]) -> Result<Vec<Edit>> {
    let mut r = Reader::new(elst);
    let version = r.version()?;
    let count = r.count(if version == 1 { 20 } else { 12 })?;
    (0..count)
        .map(|_| {
            let (duration, media_time) = if version == 1 {
                (r.u64()?, r.u64()? as i64)
            } else {
                (u64::from(r.u32()?), i64::from(r.u32()? as i32))
            };
            Ok(Edit {
                duration,
                media_time,
                rate: r.u32()? as i32,
            })
        })
        .collect()
}

/// Every sample of a track: where it is, how long it lasts, when it shows and
/// whether it starts a picture on its own. Checked to lie inside the file.
pub(super) fn sample_table(stbl: &[u8], file_len: u64) -> Result<Vec<Sample>> {
    let boxes = atoms(stbl)?;
    let find = |kind: &[u8; 4]| boxes.iter().find(|a| &a.kind == kind).map(|a| a.payload);

    let mut r = Reader::new(find(b"stsz").ok_or_else(|| anyhow!("a track has no sample sizes"))?);
    r.version()?;
    let uniform = r.u32()?;
    let count = if uniform == 0 {
        r.count(4)?
    } else {
        r.u32()? as usize
    };
    ensure!(count <= MAX_SAMPLES, "a track lists too many samples");
    let sizes: Vec<u32> = if uniform == 0 {
        (0..count).map(|_| r.u32()).collect::<Result<_>>()?
    } else {
        vec![uniform; count]
    };

    let chunks: Vec<u64> = if let Some(stco) = find(b"stco") {
        let mut r = Reader::new(stco);
        r.version()?;
        let n = r.count(4)?;
        (0..n)
            .map(|_| r.u32().map(u64::from))
            .collect::<Result<_>>()?
    } else if let Some(co64) = find(b"co64") {
        let mut r = Reader::new(co64);
        r.version()?;
        let n = r.count(8)?;
        (0..n).map(|_| r.u64()).collect::<Result<_>>()?
    } else {
        bail!("a track has no chunk offsets")
    };

    let mut r = Reader::new(find(b"stsc").ok_or_else(|| anyhow!("a track has no chunk map"))?);
    r.version()?;
    let n = r.count(12)?;
    let runs: Vec<(u32, u32)> = (0..n)
        .map(|_| {
            let first = r.u32()?;
            let per_chunk = r.u32()?;
            ensure!(
                r.u32()? == 1,
                "a track with several sample descriptions cannot be joined"
            );
            Ok((first, per_chunk))
        })
        .collect::<Result<_>>()?;

    let mut offsets = Vec::with_capacity(count);
    for (index, &(first, per_chunk)) in runs.iter().enumerate() {
        let last = runs
            .get(index + 1)
            .map_or(chunks.len() as u32 + 1, |next| next.0);
        ensure!(
            first >= 1 && first <= last,
            "a track's chunk map is out of order"
        );
        for chunk in first..last {
            let mut at = *chunks
                .get(chunk as usize - 1)
                .ok_or_else(|| anyhow!("a track's chunk map names a chunk it has not"))?;
            for _ in 0..per_chunk {
                let size = *sizes
                    .get(offsets.len())
                    .ok_or_else(|| anyhow!("a track's chunks hold more samples than it lists"))?;
                offsets.push(at);
                at += u64::from(size);
            }
        }
    }
    ensure!(
        offsets.len() == count,
        "a track's chunks hold fewer samples than it lists"
    );

    let mut r = Reader::new(find(b"stts").ok_or_else(|| anyhow!("a track has no timing"))?);
    r.version()?;
    let n = r.count(8)?;
    let mut deltas = Vec::with_capacity(count);
    for _ in 0..n {
        let (run, delta) = (r.u32()?, r.u32()?);
        ensure!(
            deltas.len() + run as usize <= count,
            "a track times more samples than it has"
        );
        deltas.extend(std::iter::repeat_n(delta, run as usize));
    }
    ensure!(
        deltas.len() == count,
        "a track times fewer samples than it has"
    );

    let mut compositions = vec![0i64; count];
    if let Some(ctts) = find(b"ctts") {
        let mut r = Reader::new(ctts);
        // Version 0 is unsigned, but writers put negative offsets in it too;
        // both are read as signed, as players do.
        r.version()?;
        let n = r.count(8)?;
        let mut at = 0usize;
        for _ in 0..n {
            let run = r.u32()? as usize;
            let offset = i64::from(r.u32()? as i32);
            ensure!(
                at + run <= count,
                "a track offsets more samples than it has"
            );
            compositions[at..at + run].fill(offset);
            at += run;
        }
    }

    let mut sync = vec![true; count];
    if let Some(stss) = find(b"stss") {
        sync.fill(false);
        let mut r = Reader::new(stss);
        r.version()?;
        let n = r.count(4)?;
        for _ in 0..n {
            let number = r.u32()? as usize;
            ensure!(
                number >= 1 && number <= count,
                "a track marks a key frame it has not"
            );
            sync[number - 1] = true;
        }
    }

    (0..count)
        .map(|i| {
            let end = offsets[i].checked_add(u64::from(sizes[i]));
            ensure!(
                end.is_some_and(|end| end <= file_len),
                "sample {} lies past the end of the file: the clip is cut short",
                i + 1
            );
            Ok(Sample {
                offset: offsets[i],
                size: sizes[i],
                delta: deltas[i],
                composition: compositions[i],
                sync: sync[i],
            })
        })
        .collect()
}

/// A clip as it is laid out on disk.
pub(super) struct Movie {
    pub(super) path: PathBuf,
    pub(super) bytes: u64,
    pub(super) ftyp: Vec<u8>,
    /// The `mvhd` payload.
    pub(super) mvhd: Vec<u8>,
    pub(super) timescale: u32,
    pub(super) tracks: Vec<Track>,
}

impl Movie {
    pub(super) fn read(path: &Path) -> Result<Self> {
        Self::read_inner(path).with_context(|| format!("{} is not a usable MP4", path.display()))
    }

    pub(super) fn read_inner(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let bytes = file.metadata()?.len();
        let mut ftyp = None;
        let mut moov = None;
        let mut at = 0u64;
        while at < bytes {
            file.seek(SeekFrom::Start(at))?;
            let mut head = [0u8; 16];
            let got = read_up_to(&mut file, &mut head)?;
            ensure!(got >= 8, "a box header is cut short");
            let kind: [u8; 4] = head[4..8].try_into()?;
            let (header, size) = match u32::from_be_bytes(head[0..4].try_into()?) {
                0 => (8, bytes - at),
                1 => {
                    ensure!(got >= 16, "a box header is cut short");
                    (16, u64::from_be_bytes(head[8..16].try_into()?))
                }
                n => (8, u64::from(n)),
            };
            ensure!(
                size >= header && at + size <= bytes,
                "the box `{}` runs past the end of the file: the clip is cut short",
                fourcc(&kind)
            );
            match &kind {
                b"moof" => bail!("fragmented MP4 is not supported"),
                b"ftyp" | b"moov" => {
                    ensure!(
                        size <= MAX_MOOV_BYTES,
                        "the `{}` box is too large",
                        fourcc(&kind)
                    );
                    let mut payload = vec![0u8; (size - header) as usize];
                    file.seek(SeekFrom::Start(at + header))?;
                    file.read_exact(&mut payload)?;
                    if &kind == b"ftyp" {
                        ftyp = Some(boxed(b"ftyp", &payload));
                    } else {
                        moov = Some(payload);
                    }
                }
                _ => {}
            }
            at += size;
        }
        let ftyp = ftyp.ok_or_else(|| anyhow!("no `ftyp` box: not an MP4 file"))?;
        let moov = moov.ok_or_else(|| anyhow!("no `moov` box: the clip is cut short"))?;
        let mvhd = child(&moov, b"mvhd")?.to_vec();
        let mut r = Reader::new(&mvhd);
        let timescale = if r.version()? == 1 {
            r.take(16)?;
            r.u32()?
        } else {
            r.take(8)?;
            r.u32()?
        };
        ensure!(timescale > 0, "the movie has no timescale");
        let mut tracks = Vec::new();
        for atom in atoms(&moov)? {
            if &atom.kind == b"trak" {
                if let Some(track) = Track::read(atom.payload, bytes)? {
                    tracks.push(track);
                }
            }
        }
        let movie = Self {
            path: path.to_path_buf(),
            bytes,
            ftyp,
            mvhd,
            timescale,
            tracks,
        };
        let video = movie.video()?;
        ensure!(!video.samples.is_empty(), "the picture track has no frames");
        ensure!(video.samples[0].sync, "the first frame is not a key frame");
        Ok(movie)
    }

    pub(super) fn video(&self) -> Result<&Track> {
        self.tracks
            .iter()
            .find(|t| t.kind == Kind::Video)
            .ok_or_else(|| anyhow!("{} has no picture track", self.path.display()))
    }

    pub(super) fn audio(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == Kind::Audio)
    }

    pub(super) fn track(&self, kind: Kind) -> Result<&Track> {
        match kind {
            Kind::Video => self.video(),
            Kind::Audio => self
                .audio()
                .ok_or_else(|| anyhow!("{} has no sound track", self.path.display())),
        }
    }
}

pub(super) fn read_up_to(file: &mut File, buf: &mut [u8]) -> Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match file.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

pub(super) fn rescale(value: u64, from: u32, to: u32) -> u64 {
    if from == to {
        return value;
    }
    ((u128::from(value) * u128::from(to) + u128::from(from) / 2) / u128::from(from)) as u64
}
