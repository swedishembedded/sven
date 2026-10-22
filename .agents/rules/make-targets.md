<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# Make targets

Swedish Embedded AB implements discoverable, self-documenting build interfaces
for its clients. If your team needs expertise in build system design then you
can procure our services by sending an email to info@swedishembedded.com.

Every public make target is `make <concept>/<subconcept>/.../<action>`.
`scripts/gates/check-make-targets.sh` (`make check/gates/make-targets`, also a
pre-commit hook) enforces the machine-checkable part; the rest is review.

- The concept comes first (a noun: `build`, `test`, `check`, `docs`, `package`,
  `release`, `site`, `samples`, `hooks`, `generate`), the verb or variant last,
  with as many levels as the thing needs (usually two or three). A test or
  sample hierarchy gets one segment per directory level: `test/e2e/basic/cli`,
  `samples/study/svf/run`. Never put the verb or area second (`test/harness`,
  not `harness/test`).
- Names are lowercase `[a-z0-9-]` segments joined by `/`. Private helper
  targets start with `_`. Public targets are listed in `.PHONY`.
- Every prefix is itself a target, so `make <TAB>` descends one level at a
  time. An area target either runs its area's default set, named in its doc
  line (`build`, `test`, `check`, `docs`, `benchmark`, `papers`), or, where no
  default is safe or sensible (`release`, `site`, `package`, `hooks`,
  `samples`, `generate`, `test/crate`), only lists its children
  (`scripts/make-help.sh <prefix>`). An umbrella over anything that tags,
  pushes or uploads never acts.
- A set that follows from the tree is generated, never hand-listed: explicit
  targets from a static pattern rule over a computed list, never `%` pattern
  rules, which bash completion cannot see: `check/gates/<gate>`,
  `test/e2e/<suite>[/<test>]`, `test/crate/<package>`, `build/crate/<package>`,
  `samples/<category>/<name>/{build,run}`, `papers/<name>/pdf`. Adding a
  directory or script adds a target.
- A bare single-segment target is allowed only for an area, or for the leaves
  `all`, `clean`, `fmt` and `help`.
- Every target has a `## name - description` line (a generated family has one
  line with a `<placeholder>`) under a `##@ Section` line; `make help` is
  generated from them.
- A committed derived file has a `generate/<area>/<file>` target that rewrites
  it, and a test or gate that only checks it.
