<!--
SPDX-License-Identifier: CC-BY-4.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Papers

`make papers/<name>/pdf` (or `make papers/pdf` for all) builds a paper into
`target/papers/<name>.pdf`.

Each paper has one question, stated as its scope in the introduction. A new
verified finding goes into the paper whose scope it falls in, in the same change
that produces it. A finding that fits no scope below starts a new paper
directory with its own scope; it is not squeezed into the nearest one.

| Directory | Scope: a finding belongs here if it changes what is claimed about, or how one would build or validate... |
|---|---|
| `state-machine/` | the methodology of building large-scale hierarchical state-machine architectures in Rust: the mathematical framework (state trees, configuration invariant, LCA transition semantics), the pure-transition / effect-executor separation and its delivery obligations, hierarchical composition, and the mechanical validation of the kernel's control flow (model checking with an expected-failure taxonomy, kernel test evidence) |
