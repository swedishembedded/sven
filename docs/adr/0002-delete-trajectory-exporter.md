# ADR 0002: Delete the sven-side trajectory exporter

## Status

Accepted. Implemented by deleting `crates/memory/src/trajectory_export.rs`
and the `sven learn export-trajectories` CLI subcommand.

## Context

`crates/memory/src/trajectory_export.rs` read sven's own recorded
`*.atif.json` trajectories, kept the ones whose stamped
`final_metrics.extra.reward` cleared a threshold, and wrote each as one row
of a hand-rolled JSONL wire format (`WireRecord`/`WireMessage`/
`WireToolCall`/`WireFunction`) mirroring brain's `data::chat::ChatSample`
input.

Its own module doc justified the hand-rolled mirror on "sven and brain do
not share a Cargo dependency." That is not the relevant fact. Brain's
`crates/atif` is a byte-for-byte vendored copy of sven's own `crates/atif`
(its doc comment: "this crate is byte-for-byte copied from
`applications/sven/crates/atif`... kept in sync manually... re-sync by
diffing this crate's `src/`/`tests/` against sven's"). Brain's
`crates/rl/src/atif.rs::ingest_dir` already:

- reads every `*.json` file directly under a directory (sven's
  `<timestamp>.atif.json` files satisfy `extension == "json"` as written,
  no rename needed),
- parses each one straight into the real `atif::Trajectory` type via serde,
  with zero conversion step,
- reads `final_metrics.extra.reward` off that same `Trajectory` via
  `trajectory_reward`, and
- walks `Trajectory::sft_steps()` itself (`to_chat_sample`) to build
  brain's `ChatSample` rows.

So the exporter's entire job - trajectory in, brain-shaped rows out - was
already done, better, on brain's side of the boundary. Auditing the
exporter itself found it was strictly worse than handing brain the raw
files:

- **Reward was filter-only.** `to_wire_record` never wrote the reward value
  into its output; brain's own `WireRecord.metadata` field (the only place
  a reward could ride along) is deserialized and then marked
  `#[allow(dead_code)]` - parsed, not used. The exporter's reward-weighted
  training signal was discarded at the brain-side parse boundary it was
  written to feed, and reconstructed for real by brain's own `ingest_dir`
  reading `final_metrics.extra.reward` directly.
- **It had already drifted from the real ATIF format it claimed to
  mirror.** `text_of` silently dropped `ContentSegment::Image` (and any
  other non-text segment) rather than erroring, so a multimodal trajectory
  exported as if it were text-only with no indication anything was lost.
- **`tools: Vec::new()` was unconditional** - the wire row never carried
  which tools were actually available in the recorded session, regardless
  of what the real trajectory recorded.

By contrast, brain's `to_chat_sample` treats a multimodal step as a hard
parse error ("multimodal message body not supported by rl::atif ingestion
yet"), which is the honest failure mode; the exporter's silent drop was a
regression relative to just handing brain the file.

## Decision

Delete `crates/memory/src/trajectory_export.rs`, its `pub mod`/re-exports
in `crates/memory/src/lib.rs`, its now-unused `atif`/`sven-session-store`
dependency declarations in `crates/memory/Cargo.toml`, and the
`sven learn export-trajectories` subcommand (`LearnCommands::
ExportTrajectories` in `src/cli/learn.rs`, its dispatch arm in
`src/run/learn.rs`).

There is no replacement conversion code to write. Sven's job in this
workflow is curation - which files, which reward threshold, which
directory to hand over - not wire-format conversion; brain's `ingest_dir`
already accepts a directory of raw `.json` trajectory files and needs
nothing sven-side to convert them.

## Consequences

- One less format to keep in sync by hand: the wire structs this module
  hand-maintained as "a contract documented at both ends" no longer need
  re-syncing against brain's `data::chat` module.
- `sven-memory`'s dependency graph shrinks by two crates (`atif`,
  `sven-session-store`) that had no other consumer in the crate.
- The "hand brain a reward-stamped trajectory corpus" workflow still needs
  a curation step - which runs directory, which reward threshold - to
  become a real CLI affordance. That is deliberately out of scope here;
  it belongs in a future `sven learn document`-style stage that selects
  and hands off raw trajectory files, not one that re-encodes them.

## Revisit trigger

Revisit only if brain's `rl::atif::ingest_dir` (or its `to_chat_sample`)
stops being able to consume sven's raw `.atif.json` files directly - e.g.
if brain drops its vendored `atif` crate, or the two `Trajectory` shapes
diverge in a way `ingest_dir` can no longer parse. Absent that, converting
on sven's side re-creates the exact drift this deletion removed.
