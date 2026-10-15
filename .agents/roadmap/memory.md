# memory

**Status: planned. Nothing below has been started.**

## Goal

Agent memory that an embedding application can host as well as the CLI can.
The host builds the access context, and memory honours it as given. Memory
records sources, units and annotations under opaque ids, with anchors that
still resolve after a source is edited. Search is lexical with no model, and
becomes hybrid once the host supplies embeddings. Indexes can be rebuilt, and
a change of embedding model starts a new index generation. Sven owns the
memory semantics. Hosts implement storage behind the same traits, and one
conformance suite checks every implementation.

## Left to do

- [ ] Add a `sven-memory-api` crate (foundation tier, no SQLite) and a
  `sven-memory-sqlite` crate (services tier).
  - The api crate holds scoped `MemoryStore`, an annotation log with
    expected-revision checks, opaque string ids, `AccessContext` and
    `IndexGeneration`.
  - It also carries an adapter conformance suite behind a feature.
- [ ] Fix the current SQLite store:
  - Scope must filter inside the query, not after `LIMIT`.
  - The `LIKE` fallback and the tag filter must escape wildcards.
  - Row errors are dropped and must surface instead.
  - The schema has no `user_version` and no migrator.
  - Row ids are shown to the model.
  - Without a home directory, the store falls back to the current directory.
  - `SessionScope` is never stamped by any caller.
- [ ] Correct `docs/17-memory.md` in the same change.
  - It describes hybrid search that does not exist.
  - It documents `db_path`/`backend` keys that nothing reads.
- [ ] Move `QuestionLedger` out of `sven-memory`, so that `sven questions`
  does not need SQLite.
- [ ] Add a `sven-sources` crate for deterministic segmentation of text,
  markdown and code.
  - Anchors are a heading path plus a content fingerprint.
  - Reanchoring is explicit: it returns unambiguous or needs-review.
  - It also holds redaction, BM25 and reciprocal-rank fusion.
  - Re-export it through `sven-sdk`.
- [ ] Add workspace identity: a worktree resolves to its main repository's
  identity, and no library code reads the current directory.
- [ ] Add `sven-sdk` host seams:
  - run and call ids on human gates, and a gate handler per agent;
  - approvals that can be parked and decided after a resume;
  - sequence numbers on events, a typed signal for dropped events, and a
    `Stream` adapter;
  - an injectable memory store;
  - a model override per agent;
  - a versioned `AgentState`;
  - `SessionEvent` marked `#[non_exhaustive]`.
  - A sample that uses only the SDK proves two sessions on one engine.
- [ ] Add migration of existing memory rows into a default personal space.
