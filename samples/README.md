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
   check/gates/samples` enforces this.
2. **The package name is derived from the path.** `samples/study/svf` is
   package `sample-study-svf`. The `samples/%/build` and `samples/%/run` rules
   depend on that mapping, and `make check/gates/samples` enforces it too.

Rule 1 is about *this* workspace: a sample may depend on whatever it needs
from outside it - that is what makes it an application rather than a test.

## Every sample is a directory

`samples/<category>/<name>/`, containing at least:

- `Cargo.toml` - package `sample-<category>-<name>`, `publish = false`
- `README.md` - what it demonstrates, how to run it, what to expect
- `src/main.rs` - the entry point

## Build and run are separate, and run does not call cargo

`make samples/<category>/<name>/build` compiles; `make samples/<category>/<name>/run` executes the
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

## Harness samples are data directories

`samples/harnesses/` is the exception to "every sample is a directory with a
`Cargo.toml`". Its named entries (`branching`, `ticket-routing`,
`scoped-agent`, `repair-loop`, `repo-repair`, `parallel-research`, `suspend-resume`,
`custom-helper`) are programmable harnesses: a YAML document, the cases that state what it must do,
and nothing to compile. They are not workspace members and `make check/gates/samples`
has nothing to say about them; they are judged by the harness test kit, which
the repository's own test (`cargo test -p sven-harness-testkit --test samples`)
and `sven harness test` both run, on mock models and fixture tools only. The
one case that names a real local model, `ticket-routing/cases/live-local.yaml`,
is opt-in (`--model` and `--model-endpoint`) and is skipped by default.

The test kit is a workspace crate, so no sample could depend on it under rule
1. `samples/harnesses/sdk-runner` is the sample that obeys the rules: a real
crate (`sample-harnesses-sdk-runner`) that runs a harness directory's simpler
cases through `sven-sdk` alone, and refuses the rest with a pointer to
`sven harness test`. See `samples/harnesses/README.md`.

## Adding one

1. Create `samples/<category>/<name>/` with the three files above.
2. Add the path to the workspace `members` list in the root `Cargo.toml`.
3. Run `make check/gates/samples`.
