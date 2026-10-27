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
server's model list and its billing apply. Sessions run under automatic
approval, as headless sessions do.

## What it does not do yet

- A session is kept in memory: after `sven rc` restarts, the next message
  starts the session again from the conversation's text, without the tool
  calls in between.
- Tool approvals are not forwarded to the server.
