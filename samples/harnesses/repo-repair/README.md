<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# repo-repair - fix a failing test with real tools

A workflow that works on a repository, not on fixtures. A soft baseline gate
(`required: false`) runs the tests; if they fail, a model-only task diagnoses
the output into a schema-checked answer (`{file, summary}`), an agent task
reads the named file and edits it with `edit_file`, and the test gate decides.
A failed edit earns one repair turn whose prompt carries the new failure
output; a second failure ends the run with `gate_failed`. An already green
repository skips both tasks and completes without a model call.

The verdict is never the agent's: the gate runs `tests/test_total.sh` and only
its exit status counts. The repair is unrolled, so the attempt budget is
visible in the document and in the step counts.

```text
baseline (soft gate) -> diagnose -> fix -> tests gate -> (done | repair -> tests gate -> (done | gate_failed))
```

`fixtures/repo` is the repository: a three-line shell function with a bug (the
shipping argument is ignored) and its tests.

## Run it

On the fixture repository (copy it somewhere you can lose):

```sh
cp -r samples/harnesses/repo-repair/fixtures/repo /tmp/repo-repair-work
sven harness validate samples/harnesses/repo-repair
sven harness run samples/harnesses/repo-repair --workspace /tmp/repo-repair-work \
    --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
    --approvals auto --api-key-file key.txt
```

`--api-key-file` is for servers that need a key (a file holding just the key,
kept outside the workspace). Exit code 0 means the tests pass after the run, 1
that the workflow failed (`gate_failed`: the edits did not fix it). A small
model is expected to fail this task often; the workflow reports that, never
success.
