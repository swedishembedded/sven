# harness v2

**Status: in progress. M1 (contracts), M2 (tasks, gates, repair, success) and M4
(iteration) have landed with ADR 0005, the v2 schema, the reference and guide
sections and two samples; what is left is below. M3 (production execution) is
another lane's.**

## Goal

A person writes the whole workflow of a long agent job (prompts, tasks, what is
verified and by what, how failures are repaired, what counts as success) in
one YAML file, and sven's existing harness machinery runs it: no second agent
loop, v1 documents unchanged. The design and its rationale are in
`docs/adr/0005-declarative-harness-v2.md`; the language is described by
`crates/harness-spec/schema/harness-workflow.schema.json`.

## Left to do

- [ ] Hosts register the two harness-only tools (`shell_gate`,
  `read_json_snapshot`) for every harness run; the fixture testkit fakes them.
  Without them every gate fails closed and every file-backed loop fails.
- [ ] Invalidation of completed items when the code they were verified against
  changes (items record the plan digest they passed under).
- [ ] A reconciliation policy for resume against a changed backlog (today: none,
  the frozen list is authoritative).
- [ ] M5/M6: `use`/`imports`, `sven harness compile|explain|inspect --graph`,
  the hardening sample on a disposable C repository, durable per-item checkpoint
  writes (`checkpoint: each_item`) from the CLI.
- [ ] Delete this file when the docs describe the finished behaviour.

## Named regression tests

`child_failure_does_not_succeed_parent`, `coverage_gate_cannot_be_bypassed`,
`backlog_mutation_does_not_skip_items`, `gate_command_exit_code_is_authoritative`,
`resume_does_not_trust_interrupted_side_effect`, `v1_compiled_plan_unchanged`
(all landed), `agent_cannot_expand_capabilities` (production lane).

## Decisions taken

- Repair is unrolled, not a `retry` (a verdict is not a step failure; ADR 0005).
- A command gate is a 13th `StepSpec` primitive, available from api_version 2.
- Template roots in v2 are `inputs`, `tasks`, `failure` (repair prompts only)
  and `item` (iteration); `facts`/`steps` are internal names a v2 author never
  writes.
