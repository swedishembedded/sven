// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("could not read image file '{0}': {1}")]
    Io(String, #[source] std::io::Error),

    #[error("unsupported image format for file '{0}'")]
    UnsupportedFormat(String),

    #[error("could not decode image '{0}': {1}")]
    Decode(String, String),

    #[error("could not encode image: {0}")]
    Encode(String),

    #[error("invalid data URL: '{0}'")]
    InvalidDataUrl(String),

    #[error("base64 decode error: {0}")]
    Base64(String),
}
