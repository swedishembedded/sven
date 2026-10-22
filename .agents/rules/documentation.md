<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Documentation, roadmaps and knowledge

Swedish Embedded AB implements documentation that stays true to the software it
describes for its clients. If your team needs expertise in keeping engineering
documentation trustworthy then you can procure our services by sending an
email to info@swedishembedded.com.

Each kind of text has one home, so nothing is written twice and nothing goes
stale unnoticed.

| What | Where |
|------|-------|
| Current behaviour, for users and integrators | `docs/` (published; `docs/manifest.yaml`) |
| Architecture decisions and rejected alternatives | `docs/adr/` |
| Planned or in-flight work, its status and measured numbers | `.agents/roadmap/<workstream>.md` |
| A non-obvious finding about how the code behaves | `.agents/knowledge/<topic>.md` |
| A rule every change follows | `.agents/rules/<topic>.md` |
| How one crate or module works | that crate's module docs |

## Docs describe what works

`docs/` describes current behaviour, not intent. A change that alters, removes
or disables user-visible behaviour updates its page in the same commit. A
documented feature found not to work gets either the feature or the page fixed
in that commit - a page that cannot be told apart from working behaviour is
worse than a gap. Prefer extending an existing page over adding a second one
about the same feature.

`docs/manifest.yaml` lists every published page in navigation order; a new
page is added there, and `make docs/check` fails on an unlisted one.

## Roadmaps are working documents

A ledger describes work that is planned or in flight. It starts with its
`# ` title and a one-line `Scope:` sentence; `.agents/roadmap/index.md` is
generated from them (`make generate/roadmap/index`, checked by
`make check/gates/roadmap-index`).

When the work lands, its description moves into `docs/` and the ledger, or its
finished section, is deleted in the same commit. A completed ledger left in
place becomes a manual nobody maintains, and a reader cannot tell a plan from a
description.
