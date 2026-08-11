// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("could not read audio file '{0}': {1}")]
    Io(String, #[source] std::io::Error),

    #[error("audio file '{path}' is {size} bytes, over the {limit} byte limit")]
    TooLarge {
        path: String,
        size: usize,
        limit: usize,
    },

    /// The extension names a real audio format that this crate cannot decode.
    ///
    /// Deliberately specific so callers can tell "we know what this is, we
    /// just have no decoder" apart from "this is not audio at all".
    #[error("{format} not supported: no decoder (only WAV/PCM is supported)")]
    UnsupportedFormat { format: String },

    #[error("not a RIFF/WAVE file: {0}")]
    NotWav(String),

    #[error("malformed WAV: {0}")]
    Malformed(String),

    #[error("unsupported WAV sample format: {0}")]
    UnsupportedSampleFormat(String),

    #[error("invalid data URL: '{0}'")]
    InvalidDataUrl(String),

    #[error("base64 decode error: {0}")]
    Base64(String),
}
