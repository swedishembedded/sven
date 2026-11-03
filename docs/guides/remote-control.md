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
`--server https://example.com` signs in to another server instead. sven opens
your browser at the server's sign-in page (and always prints the link too);
sign in if asked and approve the connection. The browser then returns to a
port sven listens on for this one sign-in on this machine (`127.0.0.1`, a
port the system picks, closed afterwards), sven redeems the one-time code it
brought, checks the key the server issued and says `Authentication
successful`. This is the OAuth authorization code grant with PKCE over a
loopback redirect (RFC 7636, RFC 8252): the key never passes through the
browser or your clipboard, and the code is worthless to anyone but this
terminal, which alone knows the secret it was bound to. sven waits five
minutes; Ctrl-C cancels. If the connection is denied or the code is refused,
run `sven auth login` again.

On a remote or headless machine (over SSH, with `--no-browser`, or when no
browser opens) sven prints a link instead:

```text
Open this link in a browser signed in to https://sven.swedishembedded.com:

https://sven.swedishembedded.com/connect/sven/authorize?response_type=code&...

Then paste the code it shows here and press Enter:
```

Open the link on any machine, approve, and paste the code the page shows.

A server that predates this sign-in (its discovery document names no
`authorization_endpoint` and `token_endpoint`) gets the older device grant
(RFC 8628): sven prints an address and a short code to type there, and waits
while you approve. A server that offers neither must be updated first.

`--server` without a scheme means `http://` for this machine
(`--server localhost:3000`) and `https://` for any other. `--name` sets what
the server calls this machine; the host name by default.

Plain `http://` is accepted only for this machine (`localhost`, `127.x.x.x`,
`::1`); any other server must be `https://`, since the token and every
conversation would otherwise cross the network in the clear. The server's
discovery document must also name its control API, model API,
sign-in (authorization, token and code-page, or device) and MCP endpoints on the server's own origin (scheme, host and port);
sven refuses to sign in when one points elsewhere, so the token and model key
cannot be sent to a third party.

The sign-in is stored in `~/.config/sven/remote.yaml` (mode 0600).
`sven auth list` shows it; `sven auth logout` forgets it. Removing the
connection on the server revokes the token.

### Connected accounts

When the server offers connected accounts (accounts such as GitHub that you
connect once on the server, under Settings > Connectors), every sven session
on this machine gets the ones you allowed on it as the MCP server
`connectors`: the TUI, headless runs, `sven acp` and `sven rc`, hosted hosts
included. On the server each account lists your computers; switch this one
on there, and its tools appear (a running TUI picks them up at once). Their tools are named
`connectors-<service>_<account>__<tool>`. sven trades this machine's key at
the server for a token that lasts minutes and reaches them with that, so the
key itself never goes to the endpoint; the accounts' own sign-ins stay on the
server. A call that
needs your approval says where to give it; approve it on the server and ask
again. `sven mcp list` shows whether `connectors` connects.
`mcp_servers.connectors` in your configuration replaces it, and
`connectors: {enabled: false}` turns it off.

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

`sven rc` stays connected: when the connection drops, or goes silent for a
minute (the server sends a keepalive every 15 seconds), it reconnects on its
own and picks up from the last command it took.

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

A command from the server that this `sven` cannot read (for example one
from a newer server) is logged and skipped, and acknowledged so the server
does not resend it; the commands after it still arrive.

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
  refused. Even inside it, sven's own directories are refused
  (`<config dir>/sven` and `~/.config/sven`: the sign-in, stored provider
  keys, configuration; `<data dir>/sven`: your chats, traces and the
  sessions `sven rc` keeps), so starting `sven rc` in your home directory
  does not expose them. So is anything that a program you run there later would
  execute with your rights: `.git` (its hooks and configuration) and sven's
  own configuration files (`.sven/`, `.sven.yaml`, `sven.yaml`), in any
  directory below the working directory.
- **Web.** `web_fetch` reaches only the public internet, never this
  machine, your LAN or a cloud metadata service (`tools.web.public_only`).
- **GDB.** The `gdb` tool runs only commands that debug the target:
  breakpoints, stepping, `info`, `print`, `x`, `set var`, a bare `load` and
  `monitor` reset, run-control, register and memory commands. Everything
  else is refused, including commands that run a program (`shell`, `run`,
  `target remote | ...`) or name a file on this machine (`file`, `dump`,
  `set logging`), as are a debug server command or gdb path the model
  spells out (`tools.gdb.allow_host_commands: false`). A server it discovers
  and the configured `gdb_path` still work.
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

## Signing in from a start credential

```bash
sven rc --login-file /run/sven/login.yaml
```

A host that nobody can sit down at, such as a container the server started
for you, cannot approve a device code. The server gives it a single-use start
credential instead, in a file:

```yaml
server: https://sven.example.com
start_credential: <opaque string>
```

The file is YAML with exactly these two fields (anything else is an error)
and at most 64 KiB. `sven rc` reads it once at startup and leaves it alone,
so it can sit on a read-only volume. `--login-file` cannot be combined with
`--hosted`; the host is an ordinary `sven rc` with the same tools and the
same limits as one signed in with `sven auth login`.

The exchange: sven fetches the server's discovery document and refuses to go
on unless its control and model URLs are on the server's own origin, as at
sign-in. It then sends the credential once, as a bearer token, to the
server's `/start` and receives this host's token, workspace and account. If
the server refuses the credential (invalid, already used or expired) `sven
rc` exits and says so; start the host again with a new credential. Network
errors and 5xx answers are retried with a growing pause for up to about two
minutes before it gives up.

Nothing is written to disk. The token lives in the process's memory only:
`sven rc --login-file` does not create or change the sign-in `sven auth
login` keeps, and the credential is never logged. The host stops cleanly on
Ctrl-C and, on Unix, on SIGTERM, the signal a container runtime sends first.

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
  offered. MCP servers in its configuration are ignored: none is started
  or called.
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
MCP servers in its configuration are ignored here too.

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
