<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# samples - standalone applications built on the sven framework

A **sample** is a complete, standalone application that demonstrates one thing
you can build with sven, built and run on its own. The framework is one thing;
the applications that show what it is for are another. A sample is not a test
and not a fixture.

Sven is a framework whose CLI is one consumer among several (see the golden
rule in `AGENTS.md`). A sample is the other half of that claim's proof: `sven
agent step` shows the surface is enough to build the CLI on, and a sample shows
it is enough to build something that is *not* the CLI on.

## The two rules

1. **A sample depends on `sven-sdk` and nothing else from this workspace.** Not
   `sven-bootstrap`, not `sven-kernel`, not `sven-machines`. If a sample needs
   something the facade does not publish, that is a finding about the facade -
   fix the facade or change the sample, never reach past it. `make
   check/samples` enforces this.
2. **The package name is derived from the path.** `samples/study/svf` is
   package `sample-study-svf`. The `samples/%/build` and `samples/%/run` rules
   depend on that mapping, and `make check/samples` enforces it too.

Rule 1 is about *this* workspace. A sample may depend on whatever it needs
from outside it - that is what makes it an application rather than a test -
subject to the exception below being the only one.

## The one exception: `samples/learning/`

[`samples/learning/`](learning/README.md) is a separate cargo workspace,
excluded from the repository root's, and its samples may depend on **brain**.
They demonstrate a model learning from its own verified experience, and brain
is what moves the weights; reaching it over a wire protocol would make each
sample mostly a demonstration of the wire protocol.

That dependency must never enter sven's own build -
`scripts/gates/check-no-brain-dependency.sh` exists because sven has to build,
test and ship on a machine with no brain checkout - which is exactly what the
separate workspace buys: nothing those samples declare can reach this
workspace's `Cargo.lock`. `make check/samples` enforces the boundary in the
other direction too, failing any sample **outside** `samples/learning/` that
declares a brain dependency.

The cost is that they are not covered by the root `make build`, `make test` or
`make check`, which is how a sample rots. `make samples/learning/check` is the
compensating control, and it skips with a stated reason when brain is absent
rather than failing a clone that does not have it.

## Every sample is a directory

`samples/<category>/<name>/`, containing at least:

- `Cargo.toml` - package `sample-<category>-<name>`, `publish = false`
- `README.md` - what it demonstrates, how to run it, what to expect
- `src/main.rs` - the entry point

## Build and run are separate, and run does not call cargo

`make samples/<path>/build` compiles; `make samples/<path>/run` executes the
binary that is already there. A sample is an ordinary executable once built -
copying it to a machine with no Rust toolchain is the whole distribution story,
and a `run` that shelled out to `cargo run` would quietly make the toolchain a
runtime dependency of every demonstration.

The cost of the split is that `run` cannot notice a stale binary, so it prints
the path it is about to execute and when that file was built.

## Samples are workspace members, and they are built and tested

Unlike some sibling repos, a sample here is **not** excluded from `make test`
or `make check`. It depends only on the SDK facade, so including it costs
almost nothing - and excluding it is how a sample rots into something that no
longer compiles against the framework it is supposed to demonstrate. A sample
that does not build is a broken sample and should fail the build.

`make build` is the exception, and not a deliberate one: it runs `cargo build`
with no `--workspace`, and the repository root is itself the `sven` package, so
it builds the binary alone. `make test` (`cargo test --workspace`) and `make
check` (clippy `--workspace --all-targets`) are what actually cover samples.

`samples/learning/` is covered by neither, being a separate workspace - see the
exception above and `make samples/learning/check`.

## Adding one

1. Create `samples/<category>/<name>/` with the three files above.
2. Add the path to the workspace `members` list in the root `Cargo.toml`.
3. Run `make check/samples`.
