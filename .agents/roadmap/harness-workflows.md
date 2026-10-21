# harness-workflows

The workflow language now covers long, multi-agent sweeps over a repository. It
has a conversation per task and per item, `do:` bodies, check tasks, typed run
variables with `capture` and the trust rule, script gates, a run scratchpad,
`on_item_failure`, trace timestamps and `--conversations`. Version 2 is the only
language, and harness test cases and the test kit are gone.
`docs/reference/harnesses.md` describes all of it; ADR 0006 records why version 1
went. This file keeps only what is still open, and what the live runs taught.

## Open

1. **A run that is stopped loses its conversations and trace file.**
   `--conversations`, `--trace` and the final report are written when the run
   ends. Run 5 was stopped with SIGTERM during an agent turn and left only the
   progress checkpoint, so the transcripts of its first two files are gone; the
   stderr trace lines (with `ts_ms`) were the only record. Write each finished
   item's conversations and trace records at its item checkpoint, the same tap
   that writes `--checkpoint-out`.
2. **An auditor that never published `findings`.** In run 5, file 2
   (`crates/ablation/src/evidence.rs`) made about 30 read calls and ended three
   times without calling `harness_vars_set`. It was reminded twice, then rolled
   back, as designed. The transcript was lost to (1). Rerun that one file with
   `--conversations`, and decide whether the cause is the prompt, context
   pressure or the repetition guard.
3. **Cost per file.** One file was completed in 27.2 min: audit 3.1, repair
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
  failed and was rolled back (open item 2). The run was then stopped to end the
  session.
- **Observability.** Variable sets never reach an executor, so the trace now
  records each one (`VariableSet`, with a preview of the value).
- **Serving.** With `--kv-cache fp8`, decode used generated attention kernels
  (~39 tok/s, SM-active 54%, memory-busy 11%). With int8 KV on brain main
  35ffb7055 it measured ~55 tok/s (SM-active 78%, memory-busy 47%). Brain's
  serving work owns the remaining gap.
