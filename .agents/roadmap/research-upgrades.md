# research-to-stack - sven share

**Status: planned. Nothing below has been started.** Update this file in the same commit that
finishes or changes an item. Sven records facts and runs attempts; the embedding application
decides what they mean. Model providers are the only other component involved.

## Goal

Give an embedding application what it needs to run many isolated attempts at one task, to see
exactly what each attempt touched, and to know which prompt each model call really saw, without
sven making any policy decision.

## Left to do

- [ ] RS-S1 (M) k independent attempts of one task with a host-supplied selector.
  - A run API that starts k attempts in isolated workspaces from one task description, returns
    every attempt's outcome and trajectory, and hands them to a selector trait the host
    implements. Sven ships no selection policy beyond "first" and "all".
  - Acceptance: with the mock provider, k=4 attempts yield 4 trajectories with distinct ids, no
    shared writable state (a file written by attempt 1 is absent in attempt 2), and the selector
    sees all four before any is discarded; cancelling one leaves the others intact. `make test`.
- [ ] RS-S2 (S) Shortcut telemetry as facts in the trajectory.
  - Record, per tool call, typed facts the host can audit: paths read or written, commands
    naming a version-control directory, and network attempts (denied or not). No judgement,
    no names of protected paths in sven: the host passes the path patterns it cares about.
  - Acceptance: a scripted run that reads a host-listed path and runs a command naming `.git`
    produces both facts in the written trajectory and in the headless CI trajectory; a run that
    does neither produces none. `make test`.
- [ ] RS-S3 (M) Compaction events and served-prompt reconstruction.
  - Each context compaction or truncation is recorded as an event (replaced span, replacement
    text digest) so the exact prompt of every later model call is reconstructible from the
    trajectory. Extends the token and sampling records already planned.
  - Acceptance: a fixture that forces a compaction round trips: rebuilding the prompt of the
    call after compaction from the trajectory gives the digest the provider received. A
    trajectory with a compaction event missing fails validation. `make test`.
- [ ] RS-S4 (S) Bounded injection of skills and memory units, with accounting.
  - A configurable cap on the number of recalled units and skills injected per turn (host
    default supplied), and the injected token count per unit recorded in the step, so the host
    can compute cost and attribute outcomes. Extends the retrieval record in the memory
    roadmap.
  - Acceptance: recalling more than the cap injects exactly the cap, in score order, and the
    record lists the dropped candidates as not injected. `make test`.
- [ ] RS-S5 (S) No automatic generation of repository context files.
  - Document in the skills and memory docs that sven does not write summary files into a
    workspace on its own, and add a test that an agent run leaves the workspace tree unchanged
    apart from the agent's own edits.
  - Acceptance: the test above. `make test`.
