<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# generic-learning

Scope: sven owns enforcement of the token and turn budget, forking from a recorded state with a named context block removed, and per-completion context provenance: the agent half of the generic learning loop. Items GL-S1 to GL-S3 and the sven-side stop list.

Source: the 2026-11-21 training-methods review (read-only advisory). The same review is recorded as `generic-learning.md` in the sibling repositories of the stack; the full plan E0-E5 is in the campaign driver's ledger.

## The generic loop and its invariants

The loop needs only the verifier, the tools, a token budget and state forks, and routes each task by its measured pass rate under the current policy, never by task content. Every repo holds the same four rules:

1. **Same observation.** A token is a training target only if it was sampled in a context the student will also see. Privileged sources (a teacher, a hinted self, an oracle solution) supply states, not text.
2. **Reasoning length is a budget.** A trial passes only if the verifier passes within `B` completion tokens and `N` turns. Native reasoning mode, one mode and one budget for teacher capture, training data and every exam arm; length is controlled by the budget and by lowest-cost selection among verified passes, never by prompt text.
3. **Evaluation that can see the effect.** At least 60 tasks over at least 20 families, a reference arm between about 0.15 and 0.85, three tiers (unseen instance, held-out variant, held-out family; only the third supports a transfer claim), wall clock an infrastructure backstop.
4. **No string matching.** No filter reads the prose of a trace or the content of a task.

## Capabilities

| Id | Capability | Status | Acceptance | Depends on |
|---|---|---|---|---|
| GL-S1 | Total completion-token cap and turn cap per session, enforced by the agent; exhaustion is reported as a typed outcome distinct from infrastructure errors | gap (open item 10: no total token cap) | a session stops at the cap with the typed outcome; a recorded request shows the cap in force | GL-R1 for the per-turn part |
| GL-S2 | Fork from a recorded state with a named context block removed (a hint, a knowledge page), the student sampling unhinted from there | partial: fork and replay exist; removing a named block does not | forking at state `s_t` with the block removed yields a session whose recorded requests lack the block's text | GL-B3 (task runner) |
| GL-S3 | Per-completion context provenance: which context blocks each completion saw, recorded with the trajectory | gap | every completion record lists its blocks; the training-data builder's same-observation check reads it structurally | none |

## Stop doing (sven)

- Turning thinking off as the default for traces (`enable_thinking = false`); length is the budget's job (the campaign driver's non-thinking-traces rule is superseded pending E2).
- Per-benchmark prompt text as the way to shape reasoning.
