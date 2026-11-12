<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# sample-agent-seat - an agent and its counterpart, a person or a simulated user

An agent works a task with a counterpart (a client, a student). The counterpart
is a `Seat`: either the person at the terminal (`--live`) or an agent played
from a `SeatSpec` YAML (`--spec`) whose hidden facts and answer key the learner
cannot see. The learner cannot tell which. The run prints one JSON document: the
conversation record with its provenance (live or simulated) and, for a
simulated seat, the checks (answer-key leaks, paraphrased leaks, facts told
before they were asked for, role breaks); and the answers to optional tests put to the counterpart outside the
conversation: `--probes` (the same questions before and after) or `--plan` (phases
`before`, `after`, `transfer`, then `distractors`, then `delayed`). A spec with
`concepts` models a student: each concept's mastery rises only on a learner turn
containing its `taught_by` phrases, the seat is told its state with every message
and the final `mastery` is in the record.

```sh
make samples/agent/seat/build
make samples/agent/seat/run ARGS="--learner-config L.yaml --spec spec.yaml --seat-config S.yaml --probes probes.yaml"
make samples/agent/seat/run ARGS="--learner-config L.yaml --live --consent --retention-days 30"
```

The two configs are ordinary sven configs; with the `mock` provider and a
`mock_responses_file` both sides are scripted, so a run takes milliseconds
(`tests/scripted.rs`). A live record must be scrubbed before any training use
and is never pooled with simulated ones: see docs/adr/0007-user-seat.md.
