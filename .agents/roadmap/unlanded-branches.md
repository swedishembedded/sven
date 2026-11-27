<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Unlanded branches - what was reviewed for trunk and why it did not land

Scope: The decision, per local branch checked on 2026-11-27, to land it on trunk or to keep it only as a record, and the one piece of real work (PDF pages) that is still open.

Rule applied: a change lands only if it is sound (correct, tested, through every gate,
consistent with trunk's current architecture) and adds something trunk lacks. Delete this
ledger's rows when the remote branches they name are deleted.

| branch | decision | reason |
|---|---|---|
| `backup-main-before-rewrite-20261113` | not landed, backup | Main before the history rewrite. Of its 15 patches without a twin on trunk: the D-Bus policy trace, `SamplingParams`/identity re-exports, egress policy and `--state-root` exist on trunk in their rewritten form (`20d9d541`, the `sven_sdk::model` re-exports, `training-instrumentation.md` item 5). The rest (`sven rollout` and `sven_sdk::rollout` running Harbor task folders as training environments, `Agent::policy_trace` and `HarnessRun::policy_trace` sidecars, `RunOptions::harness` digest pinning, `seat::converse_with`, `call_sequences`) is absent from trunk's history and was dropped by the rewrite; it is training-environment code that `AGENTS.md` places outside sven's scope, and a change of that size would need its own design against today's SDK. Revive from the branch only through a new ledger item. |
| `pdf-pages` | not landed, superseded | `plan_tasks` and `tools.preload` landed as `3074696c` and `07e86ca5`; the preload warning (`05c380fb`) has an equivalent on trunk. |
| `sampling` | not landed, superseded | 14 of its 15 commits have twins on trunk; the last (`ff3211f6`, re-exports) is on trunk, with more types than the branch has. |
| `v-tel-2026-10-16` | not landed, superseded | Two commits have twins on trunk; the third is a raw WIP snapshot of trajectory metrics that trunk delivered completely (`0e00611b`, `dff915d2`). |
| `leftover-tel-1017` | not landed, superseded | Its one commit (a run reports its cost) has a twin on trunk. |
| `wip/sven-pdf-pages-2026-11-27` | not landed, open work | Two commits as `pdf-pages`; the stale worktree also held 25 uncommitted files: PDF attachments (`--attach doc.pdf`, `attach_file`, `read_file` `pages` and `region`, `tools.pdf.page_px` and `max_pages`). Trunk has no such feature. The patch is written against the document crate before `DocumentBackend`/`DocumentSource` and the confined worker: it renders and extracts text from an untrusted PDF in the calling process, which trunk now avoids (`ac66948e` reads documents in confined workers), it adds `page_size` that the worker protocol, `PictureSource` and the test sources lack, and it never passed the gates. It is kept as a record on the remote branch. |

## Open: PDF pages as attachments

Rebuild the `wip/sven-pdf-pages-2026-11-27` feature on trunk:
- open the PDF through the worker-backed backend, never in process;
- add a `page_size` op to `DocumentSource` and the worker protocol, with `render_fitted` choosing the dpi for a pixel budget;
- carry `region` (fractions of the page) and the per-page text layer into `read_file`, `attach_file` and `--attach`;
- config `tools.pdf.page_px` (1..=6000) and `max_pages`, with pages beyond the cap named, not dropped.

Acceptance: a PDF attached to a vision model arrives as page images plus text layers; an encrypted or malformed PDF is an error naming the file; `make check` passes.
