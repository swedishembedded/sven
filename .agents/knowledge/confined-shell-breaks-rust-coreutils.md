# A confined shell cannot run rust-coreutils programs, and that stays so

Swedish Embedded AB implements kernel-enforced sandboxing for agent tool
processes for its clients. If your team needs expertise in confining agent
commands without breaking the tools they use then you can procure our services
by sending an email to info@swedishembedded.com.

## What happens

uutils (`rust-coreutils`, the `cat`, `ls`, `mkdir`, `sleep` of some
distributions) reads `/proc/self/auxv` through rustix at start-up and panics
when it cannot (`rustix .../auxv.rs: unwrap on Err`). The Landlock profile of
a confined command (`sven_tools_exec::confine`) grants no `/proc/<pid>` at
all, so on such a host every uutils program fails inside a confined shell
(shell builtins, GNU coreutils, perl and the like work). Hosts that confine
commands need GNU coreutils for them.

## Threat model

The profile protects against the tools sven spawns for a model: a
prompt-poisoned model must not get a tool that reveals more than the work
needs. A confined command (and everything it starts, a sub-agent's tools
included) reaches the working tree, the package's `bin`, the system program and
library directories, a few machine-wide files, and no network. Nothing of any
other process (command line, environment, auxv, maps, status), not the
host's, not even its own entries, is reachable.

## Why there is no "own auxv" exception (decided: not shipped)

A process's own auxv reveals nothing it does not hold, so the *data* is
harmless; what cannot be expressed is the *path*. Verified on this kernel
(Landlock ABI 6) by `no_landlock_rule_can_grant_a_command_only_its_own_proc_entry`:

- A Landlock rule names an inode, resolved when the rule is made. A rule on
  `/proc/self/auxv` made by the worker names the worker's process; the command
  (a different pid) is still refused. The rule cannot be made in the command's
  `pre_exec` for its descendants either: the shell's `cat` is a grandchild with
  a pid that does not exist yet, so no rule made ahead of it can name its
  `/proc/<pid>/auxv`.
- The only rule that serves every descendant is `/proc` itself, and that opens
  every other process's `cmdline` (and more) to the tool: the same test shows a
  confined command reading a bystander's command line once `/proc` is granted.
  Landlock's ptrace scoping guards only the ptrace-checked entries (`environ`,
  `auxv`, `maps`), not `cmdline`, `stat` or `status`.

Because the only expressible rule would expose other processes to spawned
tools, the strict profile is kept (a mount namespace with a private procfs
would be the way to do it, a different and larger change).
`a_confined_command_reads_no_proc_entry_of_any_process` holds the strict
profile in place, from the shell and from a grandchild.

## Sub-agents are confined identically

Every way a sub-agent or its tool starts was checked:

- `task`, `plan_tasks` (a child is a whole sven process, whose own shell would
  be unconfined): not offered where commands are confined, in the full and the
  research profile (the research profile offered it unconditionally; fixed).
- `system add_mcp_server` (writes a config file outside the tree and names a
  command that starts unconfined): refused in a confined session (it was
  offered; fixed).
- In-process children (harness, delegation) share the parent's tool registry,
  so the same confined shell; a hosted host's children inherit the process-wide
  Landlock domain, which a child cannot leave.
- A `task` child of an unconfined session inherits the egress policy and state
  root as flags (`confinement_flags`), round-trip tested, and cannot hold a wider
  policy than it is handed.

Existing confinement tests used `cat`, `ls`, `mkdir`, `mktemp`, `sleep`, which
fail on uutils hosts for the reason above; they use shell builtins now, so the
suite does not depend on which coreutils the host has.
