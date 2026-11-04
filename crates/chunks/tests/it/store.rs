// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Behaviour of the chunk store: addressing, dedup, staleness, the resident
//! bound and durability.

use std::fs;

use sven_chunks::{
    ChunkDraft, ChunkId, ChunkKind, ChunkStore, Freshness, FsProbe, Lifetime, Provenance,
    Sensitivity, SourceProbe, SourceVersion, StoreLimits,
};

fn draft(kind: ChunkKind) -> ChunkDraft {
    ChunkDraft {
        kind,
        provenance: Provenance {
            source_tool: "shell".into(),
            args_digest: "a1".into(),
            path: None,
            source_version: None,
            turn: 1,
        },
        priority: 0,
        sensitivity: Sensitivity::Internal,
        lifetime: Lifetime::Session,
        pinned: false,
        failure: false,
    }
}

fn open(dir: &std::path::Path, resident: usize) -> ChunkStore {
    ChunkStore::open(
        dir,
        StoreLimits {
            max_resident_bytes: resident,
            ..StoreLimits::default()
        },
    )
    .unwrap()
}

#[test]
fn identical_content_is_stored_once_under_one_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(dir.path(), 1 << 20);
    let a = store
        .put("same bytes", draft(ChunkKind::CommandOutput))
        .unwrap();
    let b = store
        .put("same bytes", draft(ChunkKind::CommandOutput))
        .unwrap();
    assert_eq!(a.chunk.id, b.chunk.id);
    assert!(!a.deduplicated);
    assert!(b.deduplicated);
    assert_eq!(store.len(), 1);
    assert_eq!(
        fs::read_dir(dir.path().join("payloads")).unwrap().count(),
        1
    );
    assert_eq!(a.chunk.content_ref.bytes, 10);
    assert_eq!(a.chunk.token_estimate, 3);
}

#[test]
fn a_chunk_id_is_not_a_path() {
    assert!(ChunkId::parse("c-0123456789abcdef").is_ok());
    for bad in [
        "../etc/passwd",
        "c-../../x",
        "c-0123",
        "C-0123456789abcdef",
        "c-0123456789abcdeg",
        "",
    ] {
        assert!(ChunkId::parse(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn raw_content_is_returned_exactly_and_by_line_range() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(dir.path(), 1 << 20);
    let id = store
        .put("one\ntwo\nthree\nfour\n", draft(ChunkKind::File))
        .unwrap()
        .chunk
        .id;
    assert_eq!(store.read(&id).unwrap(), "one\ntwo\nthree\nfour\n");
    assert_eq!(store.read_lines(&id, 2, 3).unwrap(), "two\nthree\n");
    assert_eq!(store.read_lines(&id, 4, 99).unwrap(), "four\n");
    assert!(store.read_lines(&id, 9, 10).unwrap().is_empty());
}

#[test]
fn eviction_spills_the_least_recently_used_unpinned_payload_and_never_drops_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(dir.path(), 25);
    let old = store
        .put(&"a".repeat(10), draft(ChunkKind::CommandOutput))
        .unwrap()
        .chunk
        .id;
    let mut pinned_draft = draft(ChunkKind::UserConstraint);
    pinned_draft.pinned = true;
    let pinned = store.put(&"b".repeat(10), pinned_draft).unwrap().chunk.id;
    let newer = store
        .put(&"c".repeat(10), draft(ChunkKind::CommandOutput))
        .unwrap()
        .chunk
        .id;
    assert!(store.resident_bytes() <= 25);
    assert!(
        !store.is_resident(&old),
        "the least recently used unpinned payload spills"
    );
    assert!(
        store.is_resident(&pinned),
        "a pinned payload is never evicted"
    );
    assert!(store.is_resident(&newer));
    assert_eq!(
        store.read(&old).unwrap(),
        "a".repeat(10),
        "a spilled payload is still readable"
    );
}

#[test]
fn a_pinned_payload_may_exceed_the_resident_bound() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(dir.path(), 5);
    let mut d = draft(ChunkKind::Instruction);
    d.pinned = true;
    let id = store.put(&"p".repeat(40), d).unwrap().chunk.id;
    assert!(store.is_resident(&id));
}

#[test]
fn content_over_the_chunk_limit_is_refused_not_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = ChunkStore::open(
        dir.path(),
        StoreLimits {
            max_chunk_bytes: 8,
            ..StoreLimits::default()
        },
    )
    .unwrap();
    assert!(store.put("123456789", draft(ChunkKind::File)).is_err());
    assert_eq!(store.len(), 0);
}

#[test]
fn the_store_is_restored_from_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut store = open(dir.path(), 1 << 20);
        let id = store
            .put("durable", draft(ChunkKind::TestResult))
            .unwrap()
            .chunk
            .id;
        store.set_pinned(&id, true).unwrap();
        id
    };
    let mut again = open(dir.path(), 1 << 20);
    let chunk = again.get(&id).expect("chunk survives a restart");
    assert!(chunk.pinned);
    assert_eq!(chunk.kind, ChunkKind::TestResult);
    assert_eq!(again.read(&id).unwrap(), "durable");
}

#[test]
fn a_corrupted_payload_is_an_error_not_wrong_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut store = open(dir.path(), 0);
        store
            .put("original", draft(ChunkKind::File))
            .unwrap()
            .chunk
            .id
    };
    let digest = open(dir.path(), 0)
        .get(&id)
        .unwrap()
        .content_ref
        .digest
        .clone();
    fs::write(dir.path().join("payloads").join(digest), "tampered").unwrap();
    assert!(open(dir.path(), 0).read(&id).is_err());
}

fn file_draft(path: &str, version: SourceVersion) -> ChunkDraft {
    let mut d = draft(ChunkKind::File);
    d.provenance.source_tool = "read_file".into();
    d.provenance.path = Some(path.into());
    d.provenance.source_version = Some(version);
    d
}

#[test]
fn a_file_chunk_is_stale_once_the_file_changes() {
    let root = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.txt"), "v1").unwrap();
    let probe = FsProbe::new(root.path()).unwrap();
    let version = probe.version("a.txt").unwrap().unwrap();

    let mut store = open(store_dir.path(), 1 << 20);
    let id = store
        .put("v1", file_draft("a.txt", version))
        .unwrap()
        .chunk
        .id;
    assert_eq!(store.freshness(&id, &probe).unwrap(), Freshness::Fresh);

    fs::write(root.path().join("a.txt"), "v2 longer").unwrap();
    assert!(matches!(
        store.freshness(&id, &probe).unwrap(),
        Freshness::Stale {
            current: Some(_),
            ..
        }
    ));

    fs::remove_file(root.path().join("a.txt")).unwrap();
    assert!(matches!(
        store.freshness(&id, &probe).unwrap(),
        Freshness::Stale { current: None, .. }
    ));
}

#[test]
fn a_chunk_without_a_source_has_no_freshness_to_report() {
    let root = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let probe = FsProbe::new(root.path()).unwrap();
    let mut store = open(store_dir.path(), 1 << 20);
    let id = store
        .put("out", draft(ChunkKind::CommandOutput))
        .unwrap()
        .chunk
        .id;
    assert_eq!(
        store.freshness(&id, &probe).unwrap(),
        Freshness::NotFileDerived
    );
}

#[test]
fn a_file_chunk_recorded_without_a_version_is_unverifiable_never_fresh() {
    let root = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.txt"), "x").unwrap();
    let probe = FsProbe::new(root.path()).unwrap();
    let mut d = draft(ChunkKind::File);
    d.provenance.path = Some("a.txt".into());
    let mut store = open(store_dir.path(), 1 << 20);
    let id = store.put("x", d).unwrap().chunk.id;
    assert_eq!(
        store.freshness(&id, &probe).unwrap(),
        Freshness::Unverifiable
    );
}

#[test]
fn the_probe_stays_inside_its_root() {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("jail");
    fs::create_dir(&root).unwrap();
    fs::write(outer.path().join("secret.txt"), "s").unwrap();
    let probe = FsProbe::new(&root).unwrap();
    assert!(probe.version("../secret.txt").is_err());
    assert!(probe
        .version(outer.path().join("secret.txt").to_str().unwrap())
        .is_err());
}

#[test]
fn deduplicated_content_takes_the_latest_source_version_and_the_strictest_flags() {
    let store_dir = tempfile::tempdir().unwrap();
    let mut store = open(store_dir.path(), 1 << 20);
    let v1 = SourceVersion {
        size: 1,
        mtime_ns: 1,
    };
    let v2 = SourceVersion {
        size: 1,
        mtime_ns: 2,
    };
    store.put("x", file_draft("a.txt", v1)).unwrap();
    let mut again = file_draft("a.txt", v2.clone());
    again.failure = true;
    again.sensitivity = Sensitivity::Secret;
    let chunk = store.put("x", again).unwrap().chunk;
    assert_eq!(chunk.provenance.source_version, Some(v2));
    assert!(chunk.failure);
    assert_eq!(chunk.sensitivity, Sensitivity::Secret);
}

#[test]
fn seeing_the_content_again_without_a_version_keeps_the_version_already_known() {
    let store_dir = tempfile::tempdir().unwrap();
    let mut store = open(store_dir.path(), 1 << 20);
    let v = SourceVersion {
        size: 1,
        mtime_ns: 7,
    };
    store.put("x", file_draft("a.txt", v.clone())).unwrap();
    let mut late = draft(ChunkKind::File);
    late.provenance.path = Some("a.txt".into());
    let chunk = store.put("x", late).unwrap().chunk;
    assert_eq!(chunk.provenance.source_version, Some(v));
}

#[test]
fn a_read_only_store_serves_what_was_recorded_and_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut writer = open(dir.path(), 1 << 20);
        writer
            .put("evidence", draft(ChunkKind::CommandOutput))
            .unwrap()
            .chunk
            .id
    };
    let index_before = fs::read(dir.path().join("index.jsonl")).unwrap();

    let mut reader = ChunkStore::open_read_only(dir.path(), StoreLimits::default()).unwrap();
    assert!(reader.is_read_only());
    assert_eq!(reader.read(&id).unwrap(), "evidence");
    assert!(reader.put("new", draft(ChunkKind::CommandOutput)).is_err());
    assert!(reader.set_pinned(&id, true).is_err());
    assert_eq!(reader.len(), 1);
    assert_eq!(
        fs::read(dir.path().join("index.jsonl")).unwrap(),
        index_before,
        "a read-only store never writes the index"
    );
}

#[test]
fn a_read_only_store_does_not_create_the_directory_it_is_pointed_at() {
    let parent = tempfile::tempdir().unwrap();
    let missing = parent.path().join("absent");
    assert!(ChunkStore::open_read_only(&missing, StoreLimits::default()).is_err());
    assert!(!missing.exists());
}

#[test]
fn a_record_the_writer_is_still_appending_does_not_fail_a_read_only_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut writer = open(dir.path(), 1 << 20);
        writer
            .put("whole", draft(ChunkKind::CommandOutput))
            .unwrap();
    }
    let index = dir.path().join("index.jsonl");
    let mut torn = fs::read(&index).unwrap();
    torn.extend_from_slice(b"{\"id\":\"c-00");
    fs::write(&index, torn).unwrap();

    let reader = ChunkStore::open_read_only(dir.path(), StoreLimits::default()).unwrap();
    assert_eq!(reader.len(), 1, "the torn tail is not a chunk");
    assert!(
        ChunkStore::open(dir.path(), StoreLimits::default()).is_err(),
        "a writer still refuses a damaged index"
    );
}
