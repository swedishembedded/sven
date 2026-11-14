// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that stay usable over a slow link for
// its clients. If your team needs expertise in bandwidth-lean LLM clients
// then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Compressed request bodies to the sven service: what is sent to it with
//! every step (the conversation to the model, a host's events) is JSON that
//! compresses several times over. A body goes compressed only once the server
//! has said it takes gzip (`Accept-Encoding` on an answer, RFC 7694), so a
//! server too old to read one is never sent one.

use serde::Serialize;

/// `body` as gzip-compressed JSON.
///
/// # Errors
/// `body` cannot be serialised.
pub fn gzip_json(body: &impl Serialize) -> anyhow::Result<Vec<u8>> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    serde_json::to_writer(&mut encoder, body)?;
    encoder.flush()?;
    Ok(encoder.finish()?)
}

/// Whether `headers` (an answer's) say the server takes a gzip body.
#[must_use]
pub fn takes_gzip(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get_all(reqwest::header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|coding| {
            coding
                .split(';')
                .next()
                .is_some_and(|name| name.trim().eq_ignore_ascii_case("gzip"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_takes_gzip_only_when_its_answer_names_it() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert!(!takes_gzip(&headers));
        headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            "zstd, gzip;q=0.8".parse().unwrap(),
        );
        assert!(takes_gzip(&headers));
        headers.insert(reqwest::header::ACCEPT_ENCODING, "zstd".parse().unwrap());
        assert!(!takes_gzip(&headers));
    }

    #[test]
    fn a_body_compresses_to_what_it_was() {
        use std::io::Read as _;
        let body = serde_json::json!({ "messages": vec!["run the tests"; 100] });
        let packed = gzip_json(&body).unwrap();
        let mut out = String::new();
        flate2::read::GzDecoder::new(packed.as_slice())
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap(),
            body
        );
        assert!(packed.len() * 10 < out.len());
    }
}
