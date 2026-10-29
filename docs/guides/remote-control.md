<!--
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

SPDX-License-Identifier: Apache-2.0
-->

# Remote control

Remote control lets a web application, or any other client of a
remote-control server, start agent sessions that run **on your machine**:
in your checkout, with your tools, under your permission policy. You watch
and steer them from the server's interface. The machine only ever connects
out; nothing on it listens for connections.

## Sign in once

```bash
sven auth login
```

That signs in to the hosted sven service, `https://sven.swedishembedded.com`;
`--server https://example.com` signs in to another server instead. sven
prints an address and a code, for example `Open
https://sven.swedishembedded.com/connect/sven and enter the code WDJB-MJHT`. Open the address, sign in to the server if
asked, type the code and approve the connection; sven waits for it, then
checks the key the server issued and says `Authentication successful`. The
key never passes through the browser or your clipboard, and only the terminal
that asked for the code can collect it. A code works for ten minutes; if it
expires or the connection is denied, run `sven auth login` again. Ctrl-C
cancels the wait. A server that predates this sign-in must be updated first.

`--server` without a scheme means `http://` for this machine
(`--server localhost:3000`) and `https://` for any other. `--name` sets what
the server calls this machine; the host name by default.

The sign-in is stored in `~/.config/sven/remote.yaml` (mode 0600).
`sven auth list` shows it; `sven auth logout` forgets it. Removing the
connection on the server revokes the token.

## Take sessions

```bash
cd ~/src/project
sven rc
```

`sven rc` connects and shows up on the server as connected. Every
conversation started there for this machine becomes a session here, working
in the directory `sven rc` started in (`--working-dir DIR` for another).
This machine runs each session's tool calls and file edits, and only this
machine: every other place the conversation is open - the server's
interface, `sven rc attach` - watches it and sends messages to it.

`sven rc` itself takes no input. Like `watch`, it shows what its sessions
do: one line per message, tool call and finished answer. To write to a
conversation, attach to it (below). Ctrl-C stops `sven rc` and any answer
in progress.

One `sven rc` runs every conversation of its directory at once, each a
session of its own with its own history, model, mode and tool approvals,
as if each were a separate `sven`.

A message that starts with `/` is a slash command, run as the terminal runs
it: a custom command sends its instructions, and the mode and model ones
change the session; `/clear` starts the conversation over and `/compact`
summarises it to free context (the server receives `ContextCompacted` with
the `summary`, or a notice when there was nothing to compact or a turn was
running, and then the end of the answer). `sven rc` tells the server which commands it can run
(when it connects and when asked), so the server's interface can offer
them as you type.

Each session calls models through the server, with the token, so the
server's model list and its billing apply.

Each session's conversation, tool calls included, is kept after every
finished tool call and every finished answer in
`~/.local/share/sven/remote/sessions/` (mode 0600; the platform's data
directory elsewhere). After `sven rc` restarts, a session continues from
there rather than from the server's text-only copy.

If `sven rc` stopped in the middle of an answer, the work it did is kept.
A tool call that was cut off is never run again on its own: the agent is
told it was interrupted and may or may not have taken effect. Retrying the
answer at the server continues it from where it stopped instead of starting
over.

## What the server can reach

The server chooses the tool calls and relays the approvals, so an approval
the server answers is not a safeguard of this machine. These limits are
enforced here, by default, whatever the server approves:

- **Files.** The file tools (`read_file`, `write_file`, `edit_file`,
  `find_file`, `grep` and the tools that take file paths) work only inside
  the working directory; a path that leaves it, by `..` or a symlink, is
  refused. Even inside it, sven's own directory (`<config dir>/sven`: the
  sign-in, stored provider keys, configuration) and the sessions `sven rc`
  keeps are refused, so starting `sven rc` in your home directory does not
  expose them.
- **Web.** `web_fetch` reaches only the public internet, never this
  machine, your LAN or a cloud metadata service (`tools.web.public_only`).
- **GDB.** The `gdb` tool refuses GDB commands that reach the host
  (`shell`, `!`, `pipe`, `python`, `source`, ...), a debug server command or
  gdb path the model spells out (`tools.gdb.allow_host_commands: false`);
  a server it discovers and the configured `gdb_path` still work.
- **Shell.** The `shell` tool is allowed and starts in the working
  directory, but nothing confines what a command touches. If the server must
  not have that, remove the tool (below), or run `sven rc` in a container
  with only the project mounted.

`sven rc --wide-access` lifts the first three for a session you trust the
server with entirely.

You decide which tools the server may use. The configuration `sven rc`
reads (`--config`, or the files described in the
[configuration reference](../reference/configuration.md)) takes
`tools.only`, an allow list of tool names (every other tool is removed), or
`tools.disabled`, a deny list. For a read-only machine:

```yaml
tools:
  only: [read_file, find_file, grep, web_search, web_fetch, skill]
```

## Hosted

```bash
sven rc --hosted --sandbox /srv/hosted/<account>/<conversation>
```

A server that answers conversations nobody's machine runs starts `sven rc
--hosted` for them, one per conversation, each with a sandbox directory of
its own that it never shares. Two walls keep a hosted host to itself:

- **Its tools.** Its sessions have only tools that never touch the machine
  the host runs on - web search, web fetch and finding skills. It is an
  allow list: any other tool, including one added to sven later, is never
  offered.
- **The kernel.** Before it starts anything, the host confines itself with
  Linux Landlock to its sandbox: it can read, write and create only inside
  that directory, plus read what any program needs to run and reach the
  network (TLS roots, name resolution, time zones, system libraries), and
  so can every process it starts. Another account's sandbox, home
  directories and the rest of the machine are refused by the kernel. The
  host does not start unless the kernel enforces all of it, and on other
  systems a hosted host does not start at all. The same confinement
  refuses every TCP bind and every TCP connect except to ports 443, 80 and
  53 and the ports of the sign-in's server, control and model URLs (so a
  subverted tool cannot reach a service on the machine's loopback or
  private network), and refuses signals to, and abstract unix sockets of,
  processes outside the host. That needs Landlock ABI 6 (Linux 6.12 or
  newer); an older kernel makes the host refuse to start.

- **The network.** `web_fetch` reaches only the public internet: never the
  server itself, its private networks or a cloud metadata service, checked
  on the addresses a name resolves to and on every redirect
  (`tools.web.public_only`, which any `sven` can set).
- **Its configuration.** A hosted host reads only the file `--config`
  names, never the system, user, workspace or project files it would
  otherwise find from its home and working directory, which are where a
  tenant's own files are.
- **The key.** The server writes the sign-in to the host's standard input,
  read before the confinement; it is never in a file.

Everything it keeps is in the sandbox: its kept sessions (`sessions/`) and
where its sessions work (`work/`).

Only one `sven rc` serves a directory (or a hosted account) at a time. A
second is refused while the first is connected. A host checks in every 15 s
(and at once when it takes a message); one silent for 45 s counts as gone,
its directory can be taken by the next `sven rc`, and if it comes back after
another took its place, the server refuses it and it stops.

Starting `sven rc` again in the same directory of the same machine
reconnects every conversation that ran there, even after `sven auth login`
signed the machine in again.

### In a sandbox container

```bash
sven rc --hosted --container --state /var/lib/sven --egress-socket /run/egress/egress.sock
```

A server that runs each conversation in a container of its own starts the
host there instead. The container is the boundary, so its sessions get a
shell and the file tools as well. They work in `/workspace` (the
conversation's own files) and can use `/artifacts` (the account's shared
files); the host keeps its sessions under `--state`. The sign-in is read from
standard input, as above. The container has no network: the host listens
on its loopback and passes every connection to `--egress-socket`, the
server's proxy, and points its own clients and every command it runs at
that listener (`HTTP_PROXY`/`HTTPS_PROXY`). The server decides what the
proxy reaches.

## Read a conversation

```sh
sven rc show 6f1c...e2            # the conversation as the server keeps it
sven rc show 6f1c...e2 --follow   # and what is added, from anywhere, until Ctrl-C
```

`sven rc show` prints each finished message of a conversation that runs on
this machine - written here, in a browser, or in another terminal - and a
note while the agent is answering. It keeps a replica of the conversation's
shared document and asks the server only for what the replica lacks, so
following a long conversation costs a few bytes a second. It only reads.

```sh
sven rc attach 6f1c...e2          # read it live and write to it
sven rc attach 6f1c...e2 --tui    # the same, in the full-screen UI
```

`sven rc attach` shows the conversation the same way and sends each line
you type as its next message, through the server, exactly as if it were
typed in the server's interface: it is stored there, answered on the
machine the conversation runs on, and appears in every attached terminal
and browser. A line typed while an answer is being written is refused with
a note; send it again when the answer is done. Ctrl-C or end of input
detaches; the conversation goes on.

With `--tui` the conversation opens in the full-screen UI as if it ran
here: the answer streams in with its tool calls, and a message sent from a
browser or another terminal appears as yours. Nothing runs on this machine
and nothing is saved to its session history; the model, tools and
permissions are the ones of the machine the conversation runs on, and the
status bar shows that model. A tool call waiting for permission is asked
about here as in a local session; answer it here or wherever else the
conversation is open, and the first answer counts (the other question is
withdrawn).

## Tool approvals

A tool whose policy is to ask before it runs asks the person at the server:
the call waits until it is approved or denied there, and a call nobody
answers within ten minutes is denied. The terminal prints which tool is
waiting.

The server alone answers: `sven rc` takes no input, so there is no local
approval step, and a server that approves every request (as the hosted
service's default does) approves everything the tool list allows. Treat the
approval as a convenience and rely on the limits in
[What the server can reach](#what-the-server-can-reach): the confined
defaults and your `tools.only` / `tools.disabled` allow list, which the
server cannot change.
