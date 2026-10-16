# memory

**Status: in progress. The `sven-memory-api` contract, its conformance suite,
the `sven-memory-sqlite` adapter and the `semantic_memory` tool on top of them
exist; the items below remain.**

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

- [ ] Add annotation tools (`NewAnnotation`, already defined by the contract)
  to the tool surface.
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
