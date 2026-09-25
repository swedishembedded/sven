<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# tasks - the frozen catalog

One directory per task family, shared by whichever samples use it. Families are
here rather than under each sample because several samples measure different
learning angles on the same family, and a copy per sample would be two
catalogs that drift.

```
tasks/<family>/
  family.toml     the contract: request, predicates, limits, variation knobs
  workspace/      the template the agent gets a materialised copy of
  world/          the stateful service, reachable only through the workspace CLI
  witness/        an observation-only solver: proves the task is solvable
  reference/      the intended end state: an audit fixture, never shown
  negative/       known-bad solutions that the verifier must reject
```

## How the answer is kept out of reach

The interesting property of these tasks is that they cannot be solved by
reading. That has to be a fact about the system, not a hope about the agent.

The plan was to enforce it with a namespace sandbox, mounting only the
workspace. **That is not available on every machine this has to run on** - user
namespaces are denied in the container this was built in, there is no
`bubblewrap`, and no Docker daemon - and a boundary that only exists on some
machines is not a boundary.

So the property is arranged structurally instead: **during an episode, the
answer does not exist anywhere on the filesystem.**

- The hidden runtime state - which deployment is live, which units the device
  reports in, whether the write was committed - is chosen by the lab at run
  time and handed to the world process **over an inherited pipe**. Never a
  file, never `argv`, never the environment: `/proc/<pid>/cmdline` and
  `/proc/<pid>/environ` are readable by the same user, so both would be a
  file-shaped leak wearing a different hat.
- The world holds it in memory and answers questions about it. Reading every
  byte on the machine does not reveal it; asking the world does.
- The committed instance files therefore carry the knobs that are *legitimately*
  visible (directory layout, schema shape, which defect combination) and never
  the runtime state.

What this does **not** claim: that `reference/` and `witness/` are unreadable.
They are ordinary files in a checkout. That is deliberate and harmless - the
repair to a broken program is legitimately inferable from its source, and a
generic correct program that discovers the state at run time is exactly the
behaviour these tasks reward. What must not be inferable is the state itself,
and that is what the pipe buys.

`audit` checks the arrangement rather than assuming it: the materialised
workspace is scanned for any rendering of the episode's hidden state, and an
instance whose workspace contains its own answer fails before any model is
involved.
