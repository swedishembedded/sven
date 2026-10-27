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
sven auth login --server https://example.com
```

sven prints an address. Open it, sign in to the server if asked, and approve
the connection; the page then shows a token. Paste it back into the waiting
`sven auth login` (it is not echoed). sven checks the token with the server
and says `Authentication successful`.

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
in the directory `sven rc` started in (`--working-dir DIR` for another). The
terminal prints one line per message, tool call and finished answer; the
full conversation is on the server. Ctrl-C stops `sven rc` and any answer in
progress.

Each session calls models through the server, with the token, so the
server's model list and its billing apply.

Each session's conversation, tool calls included, is kept after every
finished answer in `~/.local/share/sven/remote/sessions/` (mode 0600; the
platform's data directory elsewhere). After `sven rc` restarts, a session
continues from there rather than from the server's text-only copy.

## Type in the terminal

A line typed into the terminal running `sven rc` is the next message of the
conversation most recently given a message, or of the one named by the
start of its id: `@6f1c run the tests`. It is taken only while that
conversation is not answering, and it appears in the server's interface
with its answer, as if it had been sent there.

## Read a conversation

```sh
sven rc show 6f1c...e2            # the conversation as the server keeps it
sven rc show 6f1c...e2 --follow   # and what is added, from anywhere, until Ctrl-C
```

`sven rc show` prints each finished message of a conversation that runs on
this machine - written here, in a browser, or in another terminal - and a
note while the agent is answering. It keeps a replica of the conversation's
shared document and asks the server only for what the replica lacks, so
following a long conversation costs a few bytes a second. It only reads;
type into `sven rc` (above) to add to the conversation.

## Tool approvals

A tool whose policy is to ask before it runs asks the person at the server:
the call waits until it is approved or denied there, and a call nobody
answers within ten minutes is denied. The terminal prints which tool is
waiting.
