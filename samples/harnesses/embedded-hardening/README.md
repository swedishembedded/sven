<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# embedded-hardening - test, simplify and harden an embedded C repository

The full version 2 pipeline on a disposable firmware repository: **bootstrap**,
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

`solution/` is the reference fix (and the extra tests that take coverage over
90 percent). `cargo test --test embedded_hardening_fixture` checks that the
fixture starts red and that the solution turns every gate green.

## Offline

```sh
sven harness validate samples/harnesses/embedded-hardening
sven harness test     samples/harnesses/embedded-hardening
```

Cases: `hardens-every-module` (the whole pipeline), `a-module-that-stays-red-
stops-the-sweep` (the run fails at that module with the check's name, the rest
stay pending) and `editing-the-check-is-caught-by-its-pin`. The cases seed
`run.sh` and `pins.sha256` into the case workspace so the pins are checked for
real; the shell is a fixture.

## Production path

```sh
cp -r samples/harnesses/embedded-hardening/repo /path/to/copy
sven harness run samples/harnesses/embedded-hardening --toolset coding \
  --workspace /path/to/copy --approvals auto \
  --model openai/<model> --model-endpoint http://127.0.0.1:8788/v1 \
  --api-key-file ~/keys.json --first-byte-timeout 900 --timeout 3000 \
  --checkpoint-out checkpoint.json --trace trace.jsonl --json
```

Out of time or killed: `sven harness resume ... --checkpoint checkpoint.json`
continues at the module it was on; completed modules whose files changed since
are run again. See [the guide](../../../docs/guides/harnesses.md#walkthrough-hardening-an-embedded-repository).

## A live run

One run on a local 27B model (45 minute deadline, `--toolset coding`): it
documented the toolchain, wrote the plan and backlog, fixed `ringbuf` (the full
check) and `crc8` (the polynomial, after one repair turn), each accepted only
once the pinned module test passed, and then, on `uart_tx`, the model server
answered the repair turn with empty replies twice and the run failed with
`side_effect_failed` after 37 minutes. Coverage, architecture and production
were not reached; checking the workspace it left with the pinned `run.sh`
confirmed the two fixed modules green, `uart_tx` red, the HAL leak still there
and 87.2 percent coverage. The shipped mock cases are what covers the rest of
the pipeline; the reference fix in `solution/` is what a full pass looks like.
