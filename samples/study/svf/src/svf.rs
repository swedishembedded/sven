// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The SVF reference encoder - the ground truth this experiment grades against.
//!
//! SVF is invented for this sample. That is the point: the four values below
//! appear in no pretraining corpus, so an agent that produces a correct SVF
//! artifact either read the errata note or guessed roughly one chance in 2^32.
//! A real format, however obscure, would leave "it already knew" as an
//! unfalsifiable alternative explanation for every positive result.
//!
//! Nothing here is secret from the *experimenter* - the secret is what the
//! agent's workspace does or does not contain. See `spec/` for the two
//! documents and `main.rs` for the hygiene check that enforces the difference.

/// The 4-byte signature every SVF artifact starts with.
pub const MAGIC: [u8; 4] = [0xC3, 0x5A, 0x1F, 0x84];

/// The only version this encoder emits. Documented in the public spec, and so
/// deliberately not part of what is being tested.
pub const VERSION: u8 = 0x01;

/// Added to the payload length before it is stored in the `LENGTH` field.
pub const LENGTH_BIAS: u16 = 3;

/// CRC-8 generator polynomial, MSB-first, no reflection, no final XOR.
pub const CRC8_POLY: u8 = 0x9B;

/// CRC-8 initial register value.
pub const CRC8_INIT: u8 = 0x3F;

/// Which artifact a task asks for. Each one needs strictly more of the errata
/// than the one above it, which is what makes the score a staircase rather
/// than a single pass/fail - see `README.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Artifact {
    /// `MAGIC | VERSION | LENGTH`. Needs the signature and the length bias.
    Header,
    /// `HEADER | BODY`. Adds the body-order rule.
    Preamble,
    /// `HEADER | BODY | CHECKSUM`. Adds the CRC parameters.
    Frame,
}

impl Artifact {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Artifact::Header => "header",
            Artifact::Preamble => "preamble",
            Artifact::Frame => "frame",
        }
    }
}

/// CRC-8 over `data`, with this format's polynomial and initial value.
#[must_use]
pub fn crc8(data: &[u8]) -> u8 {
    let mut reg = CRC8_INIT;
    for byte in data {
        reg ^= byte;
        for _ in 0..8 {
            reg = if reg & 0x80 != 0 {
                (reg << 1) ^ CRC8_POLY
            } else {
                reg << 1
            };
        }
    }
    reg
}

/// The 7-byte header for a payload of `payload_len` bytes.
#[must_use]
pub fn header(payload_len: usize) -> Vec<u8> {
    let stored = (payload_len as u16).wrapping_add(LENGTH_BIAS);
    let mut out = Vec::with_capacity(7);
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&stored.to_le_bytes());
    out
}

/// The payload in SVF body order.
#[must_use]
pub fn body(payload: &[u8]) -> Vec<u8> {
    payload.iter().rev().copied().collect()
}

/// `HEADER | BODY`.
#[must_use]
pub fn preamble(payload: &[u8]) -> Vec<u8> {
    let mut out = header(payload.len());
    out.extend_from_slice(&body(payload));
    out
}

/// `HEADER | BODY | CHECKSUM`.
#[must_use]
pub fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = preamble(payload);
    let sum = crc8(&out);
    out.push(sum);
    out
}

/// The encoding of `artifact` for `payload`.
#[must_use]
pub fn encode(artifact: Artifact, payload: &[u8]) -> Vec<u8> {
    match artifact {
        Artifact::Header => header(payload.len()),
        Artifact::Preamble => preamble(payload),
        Artifact::Frame => frame(payload),
    }
}

/// Lowercase hex, no separators - the form a task asks the agent to produce.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// How each unguessable value is written when it is written down at all.
///
/// One list, two obligations, checked in both directions by the tests in
/// [`crate::docs`]: the errata must state every one of these, or arm A1 cannot
/// succeed; the public spec and an errata-free workspace must state none of
/// them, or arm A0 is not measuring anything.
///
/// Comparison is against whitespace-stripped, lowercased text, so a single
/// rendering per value covers `C3 5A 1F 84`, `c35a1f84` and a line break in the
/// middle of either. Only the high-entropy values appear here - the length bias
/// and the body-order rule are prose, and scanning for `3` or `reversed` would
/// report a leak on every document that mentions neither. See the sample's
/// README for what that leaves unchecked.
#[must_use]
pub fn secret_renderings() -> Vec<String> {
    vec![
        hex(&MAGIC),
        format!("0x{CRC8_POLY:02x}"),
        format!("0x{CRC8_INIT:02x}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_is_seven_bytes_of_signature_version_and_biased_length() {
        let h = header(64);
        assert_eq!(h.len(), 7, "header is fixed-width");
        assert_eq!(&h[..4], &MAGIC, "it opens with the signature");
        assert_eq!(h[4], VERSION);
        // Little-endian, and biased: 64 + 3 = 67 = 0x0043.
        assert_eq!(&h[5..], &[0x43, 0x00], "length is LE and carries the bias");
    }

    #[test]
    fn the_body_is_the_payload_reversed_and_the_checksum_covers_the_header() {
        let payload = [0x01, 0x02, 0x03];
        assert_eq!(body(&payload), vec![0x03, 0x02, 0x01]);

        let f = frame(&payload);
        let (covered, sum) = f.split_at(f.len() - 1);
        assert_eq!(
            sum[0],
            crc8(covered),
            "the trailer covers HEADER|BODY, not the payload alone"
        );
        assert_ne!(
            sum[0],
            crc8(&payload),
            "a checksum over the bare payload must not coincide, or the test proves nothing"
        );
    }

    #[test]
    fn each_artifact_strictly_extends_the_one_before_it() {
        let payload = b"sven";
        let h = encode(Artifact::Header, payload);
        let p = encode(Artifact::Preamble, payload);
        let f = encode(Artifact::Frame, payload);
        assert!(p.starts_with(&h), "preamble extends header");
        assert!(f.starts_with(&p), "frame extends preamble");
        assert_eq!(f.len(), p.len() + 1);
    }

    #[test]
    fn hex_is_lowercase_and_unseparated() {
        assert_eq!(hex(&MAGIC), "c35a1f84");
    }

    #[test]
    fn a_zero_length_payload_still_has_a_biased_length_field() {
        // The degenerate case is where an agent that guessed "length is the
        // payload length" would coincidentally agree with us if the bias were 0.
        let h = header(0);
        assert_eq!(&h[5..], &[0x03, 0x00]);
        assert_ne!(LENGTH_BIAS, 0, "a zero bias would make this task guessable");
    }
}
