# harness-workflows

Scope: Open work on the harness workflow language and runtime (`sven-harness-spec`, `sven-harness-runtime`, `sven harness`).

The workflow language now covers long, multi-agent sweeps over a repository. It
has a conversation per task and per item, `do:` bodies, check tasks, typed run
variables with `capture` and the trust rule, script gates, a run scratchpad,
`on_item_failure`, trace timestamps and `--conversations`. Version 2 is the only
language, and harness test cases and the test kit are gone.
`docs/reference/harnesses.md` describes all of it; ADR 0006 records why version 1
went. This file keeps only what is still open, and what the live runs taught.

## Open

1. **An auditor that never published `findings`.** In run 5, file 2
   (`crates/ablation/src/evidence.rs`) made about 30 read calls and ended three
   times without calling `harness_vars_set`. It was reminded twice, then rolled
   back, as designed. The transcript was lost when the run was stopped, before
   conversations were written at each item. Rerun that one file with
   `--conversations`, and decide whether the cause is the prompt, context
   pressure or the repetition guard.
2. **Cost per file.** One file was completed in 27.2 min: audit 3.1, repair
   12.2 (its check gate, build + lint + format, took 55 s), review 11.8 (its
   `make test` gate took 4.3 min), and the executor's commit. One failed file was
   rolled back after 11.6 min. Exploration and baseline took 14.8 min once.
   At these rates the 958 `.rs` files under `crates/` and `src/` would take
   about 13 days on one 27B server, one file at a time; this rests on n = 2
   files and is not a measured rate. The levers:
   - decode speed (brain's serving work; ~55 tok/s single-stream on int8 KV);
   - items that run concurrently (a `for_each` is serial today; it would need
     one clone per lane and a merge step);
   - a cheaper check than the project's full `make test` per file.

3. **An agent cannot author and run a harness.** A harness is a reusable,
   versioned process, but only a compiled `use:` step can start one as a
   child. An agent should be able to write a harness document in its
   workspace, compile it, run it as a child and change it after reading how it
   failed, the way it starts a subagent with instructions. Tools for an agent
   task: `harness_validate {path}` (the compile report, with the culprit
   named) and `harness_run {path, inputs}` (loads the document or package at
   `path` inside the workspace, compiles it, and starts it through the kernel's
   `ChildSpawner::spawn_harness_child` with the caller's remaining budgets as
   its ceiling and the caller's capabilities as its upper bound; the tool
   result is the child's envelope - `ok`, `value` or `code` and `message`,
   `usage` - and the path of the child's trajectory). Capability `SpawnChild`;
   the child's conversations join the run's `--conversations`. A document the
   agent wrote is untrusted: it compiles under the same validator and trust
   rule, and cannot widen what the caller holds.

## Live findings (Qwen3.8-27B served by brain, the rust-refactor sample over sven)

Each of these was fixed in the engine or the sample before the next run:

- **Run 1.** Exploration lost its history past ~48k tokens. The coding run
  built an unprobed client, so it never learned the server's 64k window; an
  overflow retry trimmed only the request it retried, and every later request
  overflowed again; the prompt invited whole-file reads. The run now uses a
  probed client, an overflow trim becomes the thread's (an emergency
  compaction), and the explore prompt reads selectively.
- **Run 2.** The sample's inventory used `head` under `pipefail`, which failed
  the gate on SIGPIPE; it now uses awk.
- **Run 3.** A gate whose test suite left a process holding its pipes reported
  no output at all, so the repair prompt was blank. `shell_gate` now keeps what
  was read.
- **Run 4.** `capture` refused a one-line count because a long stderr log had
  been cut; the gate now reports `stdout_truncated` on its own.
- **Run 5.** Exploration and baseline passed. File 1 was refactored, reviewed
  and committed by the executor: `ablation: deduplicate citation test in
  score_answer`, one file, behaviour-preserving, a correct message. File 2
  failed and was rolled back (open item 1). The run was then stopped to end the
  session.
- **Observability.** Variable sets never reach an executor, so the trace now
  records each one (`VariableSet`, with a preview of the value).
- **Serving.** With `--kv-cache fp8`, decode used generated attention kernels
  (~39 tok/s, SM-active 54%, memory-busy 11%). With int8 KV on brain main
  35ffb7055 it measured ~55 tok/s (SM-active 78%, memory-busy 47%). Brain's
  serving work owns the remaining gap.
