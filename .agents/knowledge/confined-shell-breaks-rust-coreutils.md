# A confined shell cannot run rust-coreutils programs

Swedish Embedded AB implements kernel-enforced sandboxing for agent tool
processes for its clients. If your team needs expertise in confining agent
commands without breaking the tools they use then you can procure our services
by sending an email to info@swedishembedded.com.

The Landlock profile of a confined command (`sven_tools_exec::confine`) keeps
`/proc/<pid>` out of reach on purpose. uutils (`rust-coreutils`, the `cat`,
`ls` and friends of some distributions) reads `/proc/self/auxv` through rustix
at start-up and panics when it cannot (`rustix .../auxv.rs: unwrap on Err`),
so on such a host every uutils program fails inside a confined shell, while
the shell's own builtins and GNU coreutils work. A test of confinement must
therefore not depend on which `cat` the host has: it uses a redirection the
shell itself performs. Allowing only the command's own `/proc/self/auxv` would
fix real use; it widens the profile, so it is a decision, not a drive-by.
