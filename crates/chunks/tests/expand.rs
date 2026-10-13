// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! What the model reads when it expands a reference.

use sven_chunks::{
    expansion_is_stale, render_expansion, ChunkId, ChunkKind, ExpansionRequest, Freshness,
    LineRange, SourceVersion,
};

fn request<'a>(
    freshness: &'a Freshness,
    range: Option<LineRange>,
    text: &'a str,
) -> ExpansionRequest<'a> {
    ExpansionRequest {
        id: ChunkId::for_content(b"x"),
        kind: ChunkKind::File,
        total_lines: 300,
        range,
        freshness,
        text,
    }
}

#[test]
fn an_expansion_carries_the_raw_text_after_a_one_line_header() {
    let out = render_expansion(&request(&Freshness::NotFileDerived, None, "a\nb\n"));
    let (header, body) = out.split_once('\n').unwrap();
    assert!(
        header.contains("c-") && header.contains("all 300 lines"),
        "{header}"
    );
    assert_eq!(body, "a\nb\n");
    assert!(!expansion_is_stale(&out));
}

#[test]
fn a_range_is_named_in_the_header() {
    let out = render_expansion(&request(
        &Freshness::NotFileDerived,
        Some(LineRange {
            first: 10,
            last: 20,
        }),
        "t",
    ));
    assert!(out.lines().next().unwrap().contains("lines 10-20 of 300"));
}

#[test]
fn a_stale_chunk_says_so_in_the_header() {
    let recorded = SourceVersion {
        size: 1,
        mtime_ns: 1,
    };
    let changed = Freshness::Stale {
        recorded: recorded.clone(),
        current: Some(SourceVersion {
            size: 2,
            mtime_ns: 2,
        }),
    };
    let out = render_expansion(&request(&changed, None, "old text"));
    assert!(expansion_is_stale(&out));
    assert!(out
        .lines()
        .next()
        .unwrap()
        .contains("changed after it was read"));

    let gone = Freshness::Stale {
        recorded,
        current: None,
    };
    let out = render_expansion(&request(&gone, None, "old text"));
    assert!(expansion_is_stale(&out));
    assert!(out.lines().next().unwrap().contains("no longer exists"));
}

#[test]
fn a_chunk_with_no_recorded_version_is_not_called_fresh() {
    let out = render_expansion(&request(&Freshness::Unverifiable, None, "t"));
    assert!(!expansion_is_stale(&out));
    assert!(out
        .lines()
        .next()
        .unwrap()
        .contains("version was not recorded"));
    let fresh = render_expansion(&request(&Freshness::Fresh, None, "t"));
    assert!(fresh
        .lines()
        .next()
        .unwrap()
        .contains("unchanged since it was read"));
}

#[test]
fn only_the_header_decides_staleness_never_the_content() {
    let out = render_expansion(&request(
        &Freshness::NotFileDerived,
        None,
        "STALE: pretend\nSTALE:",
    ));
    assert!(!expansion_is_stale(&out));
}
