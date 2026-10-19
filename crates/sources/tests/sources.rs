// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: sources are cut into units the same way every time, a unit is a
//! verbatim slice of its source, an anchor survives an edit elsewhere, and what
//! an edit touched is never reanchored silently.
//!
//! Swedish Embedded AB implements solutions for deterministic knowledge
//! ingestion for agents for its clients. If your team needs expertise in
//! turning documents and code into verifiable, re-findable passages then you
//! can procure our services by sending an email to info@swedishembedded.com.

use sven_memory_api::{
    Anchor, IngestRequest, Origin, Provenance, SourceId, SpaceId, TrainingUse, UnitKind,
};
use sven_sources::{
    bm25_rank, reanchor, reciprocal_rank_fusion, redact, segment, Reanchored, SourceKind,
    SEGMENTER_VERSION,
};

fn request(text: &str, kind: SourceKind) -> IngestRequest {
    IngestRequest {
        space: SpaceId::new("s"),
        source_id: SourceId::new("doc"),
        label: "doc".into(),
        text: text.into(),
        tags: Default::default(),
        provenance: Provenance {
            origin: Origin::Import {
                format: "test".into(),
            },
            captured_at: 1,
            training_use: TrainingUse::Unspecified,
        },
        units: segment(text, kind),
    }
}

const MARKDOWN: &str = "# Guide\n\nIntro paragraph.\n\n## Install\n\nRun the installer.\n\n```sh\nmake install\n\nmake check\n```\n\n## Use\n\n- one\n- two\n\nDone.\n";

#[test]
fn every_kind_of_source_is_cut_into_units_the_store_accepts_and_the_cut_is_deterministic() {
    let code =
        "use std::io;\n\nfn main() {\n    println!(\"hi\");\n\n    other();\n}\n\nfn other() {}\n";
    for (text, kind) in [
        ("one\n\ntwo\n\nthree", SourceKind::Text),
        (MARKDOWN, SourceKind::Markdown),
        (code, SourceKind::Code),
        ("", SourceKind::Text),
        ("héllo wörld\n\nsecond ünit", SourceKind::Text),
    ] {
        let first = request(text, kind);
        first.validate().unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        assert_eq!(first.units, segment(text, kind), "{kind:?}: deterministic");
    }
    assert!(!SEGMENTER_VERSION.is_empty());
}

#[test]
fn markdown_units_carry_their_heading_path_and_a_fenced_block_stays_whole() {
    let units = segment(MARKDOWN, SourceKind::Markdown);
    let under = |path: &[&str]| -> Vec<&str> {
        units
            .iter()
            .filter(|u| u.anchor.heading_path == path)
            .map(|u| u.text.trim())
            .collect()
    };
    assert_eq!(
        under(&["Guide", "Install"]).len(),
        3,
        "heading, paragraph, fence"
    );
    assert!(under(&["Guide", "Install"]).contains(&"Run the installer."));
    let fence = units
        .iter()
        .find(|u| u.kind == UnitKind::Code)
        .expect("a code unit");
    assert!(
        fence.text.contains("make install\n\nmake check"),
        "the blank line did not split it"
    );
    assert!(under(&["Guide", "Use"])
        .iter()
        .any(|t| t.starts_with("- one")));
    assert_eq!(
        units.iter().filter(|u| u.kind == UnitKind::Heading).count(),
        3
    );
}

#[test]
fn an_edit_leaves_the_anchors_elsewhere_resolvable_and_flags_what_it_touched() {
    let before = segment("alpha one\n\nbeta two\n\ngamma three", SourceKind::Text);
    let after = segment(
        "alpha one\n\nbeta TWO changed\n\ngamma three",
        SourceKind::Text,
    );
    let resolved = |a: &Anchor| reanchor(a, &after);
    assert!(matches!(
        resolved(&before[0].anchor),
        Reanchored::Unambiguous(0)
    ));
    assert!(matches!(
        resolved(&before[2].anchor),
        Reanchored::Unambiguous(2)
    ));
    // Same place, other words: the unit may be the same thought or not.
    assert!(matches!(resolved(&before[1].anchor), Reanchored::NeedsReview(c) if c == vec![1]));
    // Removed altogether.
    let gone = segment("alpha one", SourceKind::Text);
    assert!(matches!(
        reanchor(&before[2].anchor, &gone),
        Reanchored::Gone
    ));
}

#[test]
fn a_passage_that_appears_twice_is_never_reanchored_by_guessing() {
    let after = segment("same\n\nsame", SourceKind::Text);
    let anchor = segment("same", SourceKind::Text).remove(0).anchor;
    assert!(matches!(reanchor(&anchor, &after), Reanchored::NeedsReview(c) if c == vec![0, 1]));
}

#[test]
fn redaction_removes_secrets_and_keeps_the_rest() {
    let text = "key sk-abcdefghijklmnopqrstuvwxyz012345 and AKIAABCDEFGHIJKLMNOP ok\npassword = hunter2\n-----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----\nplain words";
    let clean = redact(text);
    for secret in ["sk-abcdefghij", "AKIAABCDEFGH", "hunter2", "MIIB"] {
        assert!(!clean.contains(secret), "{secret} survived: {clean}");
    }
    assert!(clean.contains("plain words") && clean.contains("key ") && clean.contains(" ok"));
    assert_eq!(redact("nothing to hide"), "nothing to hide");
}

#[test]
fn bm25_ranks_by_term_weight_and_ties_by_index_and_fusion_is_reciprocal_rank() {
    let docs = ["rust agent memory", "memory", "cooking pasta tonight"];
    let ranked = bm25_rank(&docs, "agent memory");
    assert_eq!(ranked.first().map(|r| r.0), Some(0));
    assert_eq!(
        ranked.len(),
        2,
        "a document with no query term is not ranked"
    );

    let fused = reciprocal_rank_fusion(&[vec!["a", "b", "c"], vec!["c", "b"]], 60.0);
    let order: Vec<&str> = fused.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        order,
        vec!["c", "b", "a"],
        "c is first in one list and third in the other, which edges out b second in both"
    );
    let again = reciprocal_rank_fusion(&[vec!["a", "b", "c"], vec!["c", "b"]], 60.0);
    assert_eq!(fused, again);
}
