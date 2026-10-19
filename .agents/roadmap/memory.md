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

## Done (docs/17-memory.md)

Hybrid retrieval behind the store contract with three conformance cases, the
`Embedder` seam with a local fixed model and an OpenAI-compatible one, the
`sven-sources` crate (segmentation, explicit reanchoring, redaction, BM25,
reciprocal rank fusion), stable repository/worktree identity, and a scoped store
per agent (`Agent::with_memory`), freezing a scope on a named snapshot for an
evaluation (`MemoryScope::frozen`), and the bundled `SqliteMemory` behind the
`sqlite-memory` feature of the SDK.

## Left to do

- [ ] Add annotation tools (`NewAnnotation`, already defined by the contract)
  to the tool surface.
- [ ] Re-embed existing units when an embedder is added (the contract has no unit
  listing yet; today a unit is embedded when it is ingested).
- [ ] A model override per agent.
- [ ] Measure retrieval quality with a served embedding model. The example
  `retrieval_quality` runs it; a served model needs the server to expose the
  checkpoint's embed action on `/v1/embeddings` (the brain build used for the
  first run lists Qwen3-Embedding-0.6B as a chat model and has no `embed`
  action for it, so the run could not be made).
