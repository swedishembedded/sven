<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# repo-repair - fix a failing test with real tools

A harness that works on a repository, not on fixtures: it surveys the tree
with the shell, runs the tests, has the model diagnose the failure into a
schema-checked answer (`{file, summary}`), reads the named file, lets an agent
edit it with `edit_file`, and re-runs the tests. The verdict is never the
agent's: `run_tests.sh` records `result.json`, and an independent `verify`
step (`json_predicate` on `/passed`) decides. A failed repair is retried once,
with the new failure output in the prompt; two failed edits end in the failure
`unrepaired`. An already green repository returns `already_green` without a
model call.

The retry is unrolled (`fix_first`, `fix_second`) rather than looped, so the
attempt budget is visible in the document and in the step counts.

```text
survey -> first_run -> first_check -> diagnose -> read_source -> fix_first
       -> run_first -> check_first -> (fixed | fix_second -> run_second
       -> check_second -> (fixed | unrepaired))
```

`fixtures/repo` is the repository: a three-line shell function with a bug (the
shipping argument is ignored) and its tests.

## Run it

Offline, on fixture tools (what `sven harness test` and the kit's samples test
run; the cases state what the harness is for):

```sh
sven harness test samples/harnesses/repo-repair
```

With the real tools and a scripted model (the shell, `read_file` and
`edit_file` act on a temporary copy of `fixtures/repo`; the test judges the
repository on disk):

```sh
cargo test -p sample-harnesses-sdk-runner --test real_tools
```

Against a real local model you already serve (opt-in; nothing is started for
you). The binary copies the fixture repository to a temporary directory, runs
the harness with the real tools, prints the report and re-runs the repository's
tests itself:

```sh
cargo run -p sample-harnesses-sdk-runner --bin repo-repair -- \
  --model-endpoint http://127.0.0.1:8788/v1 --model openai/Qwen/Qwen3-0.6B \
  --api-key-file key.txt
```

`--api-key-file` is for servers that need a key (a file holding just the key);
`--keep` leaves the repository on disk. Exit code 0 means the tests pass after the run, 1 that they do not, 2 a
setup error. A small model is expected to fail this task often; the harness
reports that as `unrepaired` and the exit code as 1, never as success.
