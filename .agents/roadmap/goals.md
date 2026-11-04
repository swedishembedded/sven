<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# goals

Scope: standing goals (`/goal`): what is left after the supervised goal loop, its check commands and its terminal and `sven rc` surfaces shipped.

Shipped (2026-11-04) and described in `docs/guides/goals.md`: the goal
policy (`crates/machines/src/machines/goal.rs`), its
place in the reactive agent (`goal_flow.rs`, the `Checking` state), check
commands (`VerifierSpec::Command` through the confined gate runner), the
`update_goal` report tool, and `/goal` in the TUI and `sven rc` with the
goal kept across restarts and rebuilds.

## Left

- **Waiting.** A goal whose work waits on something running (a build, a CI
  job) has no `waiting` status: the model polls in its turns. A wait on a
  live handle or a deadline (`ScheduleTimeout`) that spends no model turn is
  the next step; it needs a handle the kernel can watch.
- **Other surfaces.** Headless runs and ACP render `GoalUpdated` but take no
  `/goal`; the SDK has no goal API. A headless `--goal <objective>` with an
  exit code per outcome, and `Agent::goal(GoalCommand)` in `sven-sdk`, would
  reuse the machine unchanged.
- **Recovery guidance.** When the same blocker recurs, the model gets the
  count and is told to find another way; it gets no guidance matched to the
  kind of failure. Lessons kept only after a later turn shows the failure
  resolved, retrieved by failure kind, are the candidate; they need the
  bench below before they are trusted.
- **Measurement.** No benchmark compares the goal loop with an unsupervised
  run on the same tasks: verified completions, cost per verified completion,
  false completions, false blocks, and turns spent after a blocker was
  evident. Learned stopping stays out until such a bench shows its false
  stops on this agent's tasks.
- **Blocker identity.** The same blocker is recognised by its normalised
  first twelve words; a model that rewords it each turn resets the count,
  and only the idle and turn limits stop it then.
