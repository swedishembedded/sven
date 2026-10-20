<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Design history

Architecture decision records explain why the framework looks the way it
does. Each record describes the situation at the time it was written. They are
history, not instructions: the current behaviour of sven is described in the
guides, concepts and reference sections, and where a record and the reference
disagree, the reference is right.

| Record | Decision |
|--------|----------|
| [ADR 0001](0001-delete-graph-dsl.md) | Remove the graph-machine DSL in favour of Rust `Machine` implementations |
| [ADR 0002](0002-delete-trajectory-exporter.md) | Remove the exporter that turned trajectories into training data |
| [ADR 0003](0003-agents-as-typed-objects.md) | Treat agents as typed, resumable objects behind an SDK |
| [ADR 0004](0004-programmable-harnesses.md) | Define agent programs as YAML that compiles through the kernel |
| [ADR 0005](0005-declarative-harness-v2.md) | Add a declarative harness v2 language that lowers tasks, gates and iteration onto the v1 machinery |
