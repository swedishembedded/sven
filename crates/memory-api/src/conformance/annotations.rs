// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Cases about the annotation log.

use super::fixtures::{owner_of, source, space};
use crate::{
    AccessContext, AnnotationKind, Evidence, MemoryError, MemoryStore, NewAnnotation, Permission,
    RetrievalRequest, RevisionId, UnitAnchor,
};

fn note(target: &UnitAnchor, body: &str, expected: &RevisionId) -> NewAnnotation {
    NewAnnotation {
        target: target.clone(),
        kind: AnnotationKind::Note,
        body: body.to_owned(),
        evidence: Vec::new(),
        expected_revision: expected.clone(),
    }
}

/// Of several authors who read the same revision, one append wins and the
/// others are told the current revision to retry from; a re-ingested source
/// invalidates every outstanding revision.
pub async fn annotation_with_a_stale_expected_revision_gets_conflict_with_current<
    S: MemoryStore,
>(
    store: &S,
) {
    let ctx = owner_of(&["s"]);
    store
        .ingest(&ctx, source("s", "doc", "claim under discussion"))
        .await
        .expect("ingest");
    let target = store
        .retrieve(&ctx, RetrievalRequest::new("claim", 1))
        .await
        .expect("retrieve")
        .hits[0]
        .unit
        .to_anchor();
    let head = store
        .annotation_head(&ctx, &space("s"), &target)
        .await
        .expect("head");
    assert_eq!(
        head, target.revision,
        "an unannotated unit is at its source revision"
    );

    let first = store
        .append_annotation(&ctx, &space("s"), note(&target, "A", &head))
        .await
        .expect("A wins");
    assert_ne!(first.resulting_revision, head);
    assert_eq!(first.author, "tester");

    for loser in ["B", "C"] {
        match store
            .append_annotation(&ctx, &space("s"), note(&target, loser, &head))
            .await
        {
            Err(MemoryError::Conflict { current }) => {
                assert_eq!(current, first.resulting_revision);
            }
            other => panic!("{loser} must conflict, got {other:?}"),
        }
    }
    let mut fabricated = note(&target, "D", &first.resulting_revision);
    fabricated.evidence = vec![Evidence {
        unit: target.clone(),
        span: "words the unit never held".to_owned(),
    }];
    assert!(
        matches!(
            store.append_annotation(&ctx, &space("s"), fabricated).await,
            Err(MemoryError::InvalidInput(_))
        ),
        "evidence must be a verbatim passage of its unit"
    );
    let mut grounded = note(&target, "B again", &first.resulting_revision);
    grounded.evidence = vec![Evidence {
        unit: target.clone(),
        span: "under discussion".to_owned(),
    }];
    let second = store
        .append_annotation(&ctx, &space("s"), grounded)
        .await
        .expect("a retry from the current revision succeeds");

    let log = store
        .annotations(&ctx, &space("s"), &target)
        .await
        .expect("annotations");
    let bodies: Vec<_> = log.iter().map(|a| a.content.body.as_str()).collect();
    assert_eq!(
        bodies,
        ["A", "B again"],
        "the log is append-only and ordered"
    );
    assert_eq!(
        store
            .annotation_head(&ctx, &space("s"), &target)
            .await
            .expect("head"),
        second.resulting_revision
    );

    let edited = store
        .ingest(&ctx, source("s", "doc", "claim under discussion, revised"))
        .await
        .expect("re-ingest");
    let current = edited.revision().clone();
    match store
        .append_annotation(
            &ctx,
            &space("s"),
            note(&target, "late", &second.resulting_revision),
        )
        .await
    {
        Err(MemoryError::Conflict { current: reported }) => assert_eq!(reported, current),
        other => panic!("an annotation on a replaced revision must conflict, got {other:?}"),
    }

    let reader = AccessContext::new("reader").reading(space("s"));
    assert!(matches!(
        store
            .append_annotation(&reader, &space("s"), note(&target, "x", &current))
            .await,
        Err(MemoryError::Forbidden(_))
    ));
    let no_annotate = AccessContext::new("w").writing(space("s"), &[Permission::Ingest]);
    assert!(matches!(
        store
            .append_annotation(&no_annotate, &space("s"), note(&target, "x", &current))
            .await,
        Err(MemoryError::Forbidden(_))
    ));
}
