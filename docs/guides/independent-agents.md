# Independent agents

An embedding application can run sven as an independent actor on a network it
controls: several agents on one machine, each with its own identity, state and
view of the network, none able to reach anything the application did not
give it. This page is what to set, what it guarantees and what it does not
cover.

## Control the network

Put a `network` section in the agent's configuration (or use the flags):

```yaml
network:
  allow_hosts: ["ntfy.sh", "*.paste.test", "a2a.peer.test:8443"]
  proxy: http://127.0.0.1:3128       # optional
  ca_bundle: /etc/emulation/ca.pem   # optional
  resolve:                           # optional
    ntfy.sh: 127.0.0.1
    paste.test: 127.0.0.1
```

| Goal | Setting |
|------|---------|
| Reach only these hosts | `allow_hosts` (`--allow-host`, repeatable). Present means exclusive: an empty list (`--deny-egress`) refuses everything |
| Send traffic through your proxy | `proxy` (`--egress-proxy`). The proxy resolves destination names |
| A proxy that terminates TLS for emulated names | `ca_bundle` (`--ca-bundle`): its authorities are trusted in addition to the built-in roots; certificates are still verified |
| Familiar names reaching local emulators | `resolve` (`--resolve ntfy.sh=127.0.0.1`), for direct connections; the port is the URL's |

Every HTTP client of the process enforces the policy at the connection layer,
not in the prompt: `web_fetch`, `web_search`, [`http_request`](../reference/tools.md#http_request),
HTTP MCP servers, HTTP verifiers. A destination outside `allow_hosts` is never
connected to, an IP literal and a redirect target included, and the tool
answers with a refusal naming the host and the key that would allow it
(`network egress to 'example.com:443' refused: not in network.allow_hosts`).
System proxy variables are ignored once a policy is in place. A sub-agent
started by `task` runs with the same state root and policy as its parent.

Not covered, by design:

- **The model provider.** Its endpoint is configured separately (`model`,
  `providers`) and is exempt, so the agent can always reach its model.
- **Shell commands and stdio MCP servers.** They are processes sven starts,
  not HTTP clients of sven, and can open any socket their user can. Contain
  them with the sandbox or network namespace you run sven in, and when the
  agent needs no commands at all, remove the tool: `--disable-tool shell`
  (`tools.disabled: [shell]`).
- **One policy per process.** The policy is process-wide; embed engines with
  different policies in different processes. A second, different policy is
  refused.

A malformed `network` section, an unreadable `ca_bundle` or a conflicting
policy stops the run at startup instead of degrading silently.

## Give each agent its own state

`--state-root DIR` keeps everything a process reads or writes for itself under
`DIR`, created private to the user:

| Under `DIR` | What |
|-------------|------|
| `config/` | `config/sven/config.yaml` is the agent's user configuration; credentials and MCP sign-ins |
| `data/` | conversations, trajectories, served skills |
| `state/` | audit log, task worktrees |
| `home/` | what `~` means, including the persistent memory (`home/.config/sven/memory`; `tools.memory.memory_file` names another file) |
| `tmp/` | scratch files |

With a root, `/etc/sven` and the user's own configuration are not read, so two
agents with different roots and working directories share no file, whatever
`HOME` and `XDG_*` say. Project files (`.sven/` in the working directory) belong
to each agent's working directory; give every agent its own. A sub-agent is
started with its parent's root.

From the SDK, call `sven_roots::set_state_root` before building the engine.
The model provider's local server socket (a brain on the same machine) is
deliberately shared and not part of the root.

## Checklist for one agent

```sh
sven --headless --state-root /run/agents/a1 --config a1.yaml \
     --allow-host ntfy.sh --resolve ntfy.sh=127.0.0.1 \
     --disable-tool shell "wait for a message on topic a1, then answer it"
```

and run it in the namespace that carries only the emulated network. Enforcing
the network at the process boundary is the embedder's; sven guarantees only
what its own tools do.

## Confined commands and sub-agents

Where sven confines the commands of a session (a served run, `sven rc
--hosted`), the threat model is a prompt-poisoned model: a tool spawned for it
must not reveal more than the work needs. Its commands reach the working
tree, the package's tools, the system program directories and a few
machine-wide files, and no network. No `/proc/<pid>` entry of any process is
readable: not another process's command line, environment or auxv, not the
host's, not the command's own. The same confinement holds for everything a
command starts, a sub-agent's tools included. A confined session can delegate:
the `task` child is a whole `sven` process, started inside the parent's Landlock
confinement (the same directory tree, no other path, and a state root of its
own inside the parent's private tmp), with the parent's egress policy and the
model endpoint it was handed. The kernel inherits the confinement to everything
the child starts and lets it only narrow, so the child cannot loosen it. The
child process may connect only to the ports of its model endpoint and of the
hosts the egress policy lists; the commands its own tools start are confined
like the parent's commands, with no network at all. `system` cannot add MCP
servers in a confined session (a server is a command started outside the
confinement); a sub-agent of an unconfined session is started with its parent's
state root and egress policy.

One consequence: programs of the `rust-coreutils` (uutils) set read their own
`/proc/self/auxv` and abort inside a confined shell. A host that confines
commands needs GNU coreutils for them. Allowing only the command's own auxv
cannot be expressed in Landlock without opening other processes' entries, so
it is not offered; the reasoning is in
`.agents/knowledge/confined-shell-breaks-rust-coreutils.md`.
