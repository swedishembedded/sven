# harness v2

**Status: in progress. M1 (contracts) landed with ADR 0005 and the v2 schema;
M2 (tasks, gates, repair, success) and M4 (iteration) are the spec lane's;
M3 (production execution) is another lane's.**

## Goal

A person writes the whole workflow of a long agent job (prompts, tasks, what is
verified and by what, how failures are repaired, what counts as success) in
one YAML file, and sven's existing harness machinery runs it: no second agent
loop, v1 documents unchanged. The design and its rationale are in
`docs/adr/0005-declarative-harness-v2.md`; the language is described by
`crates/harness-spec/schema/harness-workflow.schema.json`.

## Left to do

- [ ] M2: version dispatch before deserialization; typed v2 AST with strict
  keys; authoring validation; lowering of tasks, follow-ups, gates, repair and
  success onto v1 steps; the `gate` primitive (spec, validator, runtime,
  authorization, trace, tests); `shell_gate` in `sven-tools-exec`; delegate
  `propagate_failure`; `harness-plan/2` with a source map; a fixture-mode sample
  with cases; reference and guide pages.
- [ ] M4: `iterate` (for_each over a frozen snapshot, until a metric
  threshold), work-item state in the run state, resume and invalidation, the
  100-item interrupted workload test.
- [ ] M5/M6 (other work): `use`/`imports`, `sven harness compile|explain|
  inspect --graph`, the hardening sample on a disposable C repository.
- [ ] Delete this file when the docs describe the finished behaviour.

## Named regression tests

`child_failure_does_not_succeed_parent`, `coverage_gate_cannot_be_bypassed`,
`backlog_mutation_does_not_skip_items`, `gate_command_exit_code_is_authoritative`,
`resume_does_not_trust_interrupted_side_effect`, `v1_compiled_plan_unchanged`
(landed), `agent_cannot_expand_capabilities` (production lane).

## Decisions taken

- Repair is unrolled, not a `retry` (a verdict is not a step failure; ADR 0005).
- A command gate is a 13th `StepSpec` primitive, available from api_version 2.
- Template roots in v2 are `inputs`, `tasks`, `failure` (repair prompts only)
  and `item` (iteration); `facts`/`steps` are internal names a v2 author never
  writes.
