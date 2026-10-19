<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Contributing

sven is a Rust workspace. [AGENTS.md](../../AGENTS.md) is the authoritative
guide to the repository layout, the crate tiers, the conventions every change
follows and the gates it must pass; this page is the short path to a working
checkout.

## Build and test

```bash
make build            # debug build of every binary
make test             # unit and integration tests for the workspace
make check            # rustfmt, text gates, architecture ratchet, clippy -D warnings, rustdoc links
make help             # every target, as a tree
```

Run `make <TAB>` to descend the target tree one level at a time. Name the
package when you only touched one crate (`cargo test -p sven-config`).

`architecture.toml` at the repository root assigns every crate to a tier and
lists which dependencies are legal; `make check/arch` fails on an illegal edge.

## Documentation

The documentation in `docs/` describes current behaviour. A change that
alters user-visible behaviour updates its page in the same commit.

`docs/manifest.yaml` lists every published page, in navigation order. That
one list drives the published site, the single-file manual and the PDF. A
Markdown file under `docs/` is either listed there or matched by its
`exclude` section; `make docs/check` fails otherwise.

```bash
make docs/build       # publication in build/docs (pages, assets, manifest.json)
make docs/check       # build twice, verify links, anchors, coverage, determinism
make docs/pdf         # manual as PDF in target/docs (needs pandoc and xelatex)
make docs/clean       # remove generated documentation output
```

The publisher is vendored in `tools/docs-publisher/` and pinned by a
checksum. Pass `DOCS_PUBLISHER=<path>` to try a different build of it.

Links between pages are ordinary relative Markdown links, so the pages read
well on GitHub. Link to other projects with their canonical
`https://swedishembedded.com/docs/<project>` URL.

## Benchmarks

The [ablation benchmarks](ablation-benchmarks.md) and the
[decision experiments](decision-experiments.md) measure whether a change to
context handling pays for itself.
