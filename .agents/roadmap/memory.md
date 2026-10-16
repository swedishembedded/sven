# memory

**Status: in progress. The `sven-memory-api` contract, its conformance suite
and the `sven-memory-sqlite` adapter exist; the items below remain.**

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

- [ ] Move the `semantic_memory` tool onto `MemoryStore` and retire the old
  `sven-memory` SQLite store. The contract and `sven-memory-sqlite` already
  avoid what the old store gets wrong, so the move removes these defects:
  - scope filters after `LIMIT`, and `SessionScope` is never stamped by any
    caller;
  - the `LIKE` fallback and the tag filter do not escape wildcards;
  - row errors are dropped;
  - the schema has no `user_version` and no migrator;
  - row ids are shown to the model;
  - without a home directory the store falls back to the current directory.
- [ ] Add annotation tools and the retrieval record (`RetrievalRecord`,
  `NewAnnotation`, both already defined by the contract) to the tool surface.
- [ ] Add a `sven-sources` crate for deterministic segmentation of text,
  markdown and code.
  - Anchors are a heading path plus a content fingerprint.
  - Reanchoring is explicit: it returns unambiguous or needs-review.
  - It also holds redaction, BM25 and reciprocal-rank fusion.
  - Re-export it through `sven-sdk`.
- [ ] Add workspace identity: a worktree resolves to its main repository's
  identity, and no library code reads the current directory.
- [ ] Add the remaining `sven-sdk` host seams:
  - an injectable memory store;
  - a model override per agent.
- [ ] Have the memory tool report each retrieval through
  `sven_sdk::tool::record_memory_retrieval`, with the unit ids, revisions,
  anchors, scores and index generation of the scoped store, so
  `extra.sven.memory` is written for real memory use. The trajectory side is
  done (see `docs/technical/sdk.md`); only a test tool reports retrievals
  today.
- [ ] Add migration of existing memory rows into a default personal space.
