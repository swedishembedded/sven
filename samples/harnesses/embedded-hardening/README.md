<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# embedded-hardening - test, simplify and harden an embedded C repository

The full pipeline on a disposable firmware repository: **bootstrap**,
**plan**, **modules** (a `for_each` over a frozen backlog with `checkpoint:
each_item` and `covers`), **coverage** (`until` 90 percent), **architecture**,
**production**, and a success condition that re-measures everything.

`repo/` is the fixture, a copy of which is the workspace:

- `src/ringbuf`, `src/crc8`, `src/uart_tx` with one real defect each (an
  off-by-one full check, the wrong CRC polynomial, partial UART writes
  ignored), and `tests/test_*.c`, the specification, which fail on purpose.
- `include/hal.h` and `hal/hal_stub.c`: the HAL dependency and its host
  stand-in (a UART FIFO that takes four bytes per call). `src/uart_tx.h`
  includes `hal.h`, which the layering gate forbids.
- `.hardening/run.sh`: `build`, `test [module]`, `coverage` (gcov through
  gcovr, writing `.hardening/coverage.json`), `lint` (clang-tidy) and
  `layering`. It needs `cc`, `gcovr` and `clang-tidy`.
- `.hardening/pins.sha256`: the hashes of the given tests, checked by `build`.

The workflow pins `run.sh` and `pins.sha256` by hash (`artifact` gates with
`sha256`), so the agent can neither weaken the check nor edit the tests it is
judged by; new tests go in `tests/extra_<module>.c`, which `run.sh` also runs.
Nothing the agent says counts: only the gates.

`cargo test --test embedded_hardening_fixture` checks that the workflow pins the
hashes of the check and of the given tests, and that the build refuses a
weakened test.

## Run it

```sh
cp -r samples/harnesses/embedded-hardening/repo /path/to/copy
sven harness run samples/harnesses/embedded-hardening \
  --workspace /path/to/copy --approvals auto \
  --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
  --api-key-file ~/keys.json --first-byte-timeout 900 --timeout 3000 \
  --checkpoint-out checkpoint.json --trace trace.jsonl --json
```

Out of time or killed: `sven harness resume ... --checkpoint checkpoint.json`
continues at the module it was on; completed modules whose files changed since
are run again. See [the guide](../../../docs/guides/harnesses.md#walkthrough-hardening-an-embedded-repository).

## A live run

Two live runs on a local 27B model . The first failed after
37 minutes at the third module: the model spent its whole 4096-token output
budget thinking, answered nothing, and the run ended as `side_effect_failed`.
That led to three changes: an empty reply is asked again and then ends as
`empty_reply` (never a side effect), a server failure that may pass is asked
again after a growing pause, and this workflow sets `turn.max_output_tokens`.

The second run **completed**: 56 minutes, 120 steps, 1.2 million input and 48
thousand output tokens, 35 gates, three items completed (each accepted only by
the pinned gates), eight repair turns. The workspace it left was checked
independently with the pinned `run.sh`: build, every module's tests, layering
and lint pass, coverage is 100 percent, and `run.sh` and `pins.sha256` are
byte-identical to the shipped files. The agent changed four sources, wrote an
extra test file per module and a changelog entry.
