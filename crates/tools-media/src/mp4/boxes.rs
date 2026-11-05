// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The box structure of an MP4 file: reading boxes out of a buffer and
//! writing new ones.

use anyhow::{anyhow, ensure, Result};

/// The most entries one sample table may hold.
pub(super) const MAX_SAMPLES: usize = 10_000_000;

/// One box: its type and its payload (after the header).
pub(super) struct Atom<'a> {
    pub(super) kind: [u8; 4],
    pub(super) payload: &'a [u8],
}

/// The boxes laid one after another in `buf`.
pub(super) fn atoms(mut buf: &[u8]) -> Result<Vec<Atom<'_>>> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        ensure!(buf.len() >= 8, "a box header is cut short");
        let size32 = u32::from_be_bytes(buf[0..4].try_into()?);
        let kind: [u8; 4] = buf[4..8].try_into()?;
        let (header, size) = match size32 {
            0 => (8, buf.len() as u64),
            1 => {
                ensure!(buf.len() >= 16, "a box header is cut short");
                (16, u64::from_be_bytes(buf[8..16].try_into()?))
            }
            n => (8, u64::from(n)),
        };
        ensure!(
            size >= header as u64 && size <= buf.len() as u64,
            "the box `{}` claims more bytes than it has",
            fourcc(&kind)
        );
        let size = size as usize;
        out.push(Atom {
            kind,
            payload: &buf[header..size],
        });
        buf = &buf[size..];
    }
    Ok(out)
}

pub(super) fn child<'a>(buf: &'a [u8], kind: &[u8; 4]) -> Result<&'a [u8]> {
    atoms(buf)?
        .into_iter()
        .find(|atom| &atom.kind == kind)
        .map(|atom| atom.payload)
        .ok_or_else(|| anyhow!("no `{}` box", fourcc(kind)))
}

pub(super) fn fourcc(kind: &[u8]) -> String {
    kind.iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

/// Big-endian reads over a payload, refusing to run past its end.
pub(super) struct Reader<'a> {
    pub(super) buf: &'a [u8],
    pub(super) at: usize,
}

impl<'a> Reader<'a> {
    pub(super) fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }
    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|&end| end <= self.buf.len());
        let end = end.ok_or_else(|| anyhow!("a box is cut short"))?;
        let bytes = &self.buf[self.at..end];
        self.at = end;
        Ok(bytes)
    }
    pub(super) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub(super) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    pub(super) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into()?))
    }
    /// A full box's version, its flags skipped.
    pub(super) fn version(&mut self) -> Result<u8> {
        let version = self.u8()?;
        self.take(3)?;
        Ok(version)
    }
    /// A count of entries that each take `width` bytes, checked against what
    /// is left so a lying count cannot ask for memory the box does not hold.
    pub(super) fn count(&mut self, width: usize) -> Result<usize> {
        let count = self.u32()? as usize;
        ensure!(
            count <= MAX_SAMPLES && count.saturating_mul(width) <= self.buf.len() - self.at,
            "a table claims more entries than it holds"
        );
        Ok(count)
    }
}

pub(super) fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 16);
    if payload.len() as u64 + 8 > u64::from(u32::MAX) {
        out.extend(1u32.to_be_bytes());
        out.extend(kind);
        out.extend((payload.len() as u64 + 16).to_be_bytes());
    } else {
        out.extend((payload.len() as u32 + 8).to_be_bytes());
        out.extend(kind);
    }
    out.extend(payload);
    out
}

pub(super) fn full_box(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut head = flags.to_be_bytes();
    head[0] = version;
    boxed(kind, &[head.as_slice(), payload].concat())
}
