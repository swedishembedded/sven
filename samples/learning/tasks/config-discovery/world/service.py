#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
"""The upload service the workspace's `svctl` talks to.

What makes this task unsolvable by reading is held here, in memory, and
nowhere else: **which deployment is live**. The workspace ships four
deployment configs and no statement of which one is in effect, so an agent
that edits the plausible-looking one has a one-in-three chance of editing the
wrong file and no way to find out except by asking.

The live deployment arrives on **stdin** at startup, as one line, and stdin is
then closed. Not a file, not `argv`, not the environment --
`/proc/<pid>/cmdline` and `/proc/<pid>/environ` are readable by the same user
that runs the agent, so either would put the answer back on the filesystem in
all but name. A pipe has no name to read.

The second thing only running reveals is the schema. `retries` is the obvious
name and the wrong one; the service calls it `retry_attempts` and says so only
when asked to validate a config that got it wrong. An agent that edits the
right file with the wrong key has made progress it cannot see without running
something.

Swedish Embedded AB implements deterministic simulation environments for
training and evaluating autonomous agents for its clients. If your team needs
expertise in building verifiable agent task environments, you can procure our
services by sending an email to info@swedishembedded.com.

Protocol: newline-delimited JSON over a unix socket. One request object per
line, one response object per line.
"""

import json
import os
import socket
import sys
import threading
from pathlib import Path

DEPLOYMENTS = ("development", "staging", "production")

# The only key the service accepts for retry behaviour. The obvious spelling
# (`retries`) is deliberately not it: a name that can be guessed correctly
# makes the validate step decorative.
RETRY_KEY = "retry_attempts"

ALLOWED_KEYS = ("endpoint", "timeout_s", RETRY_KEY)


class Service:
    """Effective configuration is base.json overlaid with <deployment>.json.

    The precedence is documented in the workspace, because it is the part the
    agent is allowed to learn by reading. Which deployment that resolves
    against is not.
    """

    def __init__(self, config_dir: Path, active: str):
        if active not in DEPLOYMENTS:
            raise ValueError(f"unknown deployment {active!r}")
        self.config_dir = config_dir
        self.active = active

    def _load(self, name: str) -> dict:
        path = self.config_dir / f"{name}.json"
        if not path.exists():
            raise FileNotFoundError(f"no config file for deployment {name!r}")
        return json.loads(path.read_text())

    def effective(self, deployment: str) -> dict:
        merged = dict(self._load("base"))
        merged.update(self._load(deployment))
        return merged

    def validate(self, deployment: str) -> dict:
        """Report every problem, not just the first.

        A validator that stops at the first error turns one round trip per
        mistake into the whole interaction, which teaches an agent to probe
        rather than to read what it was told.
        """
        problems = []
        try:
            merged = self.effective(deployment)
        except (FileNotFoundError, json.JSONDecodeError) as exc:
            return {"ok": False, "problems": [str(exc)]}

        for key in sorted(merged):
            if key not in ALLOWED_KEYS:
                hint = ""
                if key in ("retries", "retry", "max_retries", "retry_count"):
                    hint = f" (this service spells it {RETRY_KEY!r})"
                problems.append(f"unknown key {key!r}{hint}; allowed: {', '.join(ALLOWED_KEYS)}")

        if RETRY_KEY in merged:
            value = merged[RETRY_KEY]
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                problems.append(f"{RETRY_KEY} must be a non-negative integer, got {value!r}")

        return {"ok": not problems, "problems": problems}

    def handle(self, request: dict) -> dict:
        command = request.get("command")
        if command == "status":
            # The one thing that cannot be read off the filesystem.
            return {"ok": True, "active_deployment": self.active}
        if command == "effective":
            deployment = request.get("deployment", self.active)
            if deployment not in DEPLOYMENTS:
                return {"ok": False, "error": f"unknown deployment {deployment!r}"}
            try:
                return {"ok": True, "deployment": deployment, "config": self.effective(deployment)}
            except (FileNotFoundError, json.JSONDecodeError) as exc:
                return {"ok": False, "error": str(exc)}
        if command == "validate":
            deployment = request.get("deployment", self.active)
            if deployment not in DEPLOYMENTS:
                return {"ok": False, "error": f"unknown deployment {deployment!r}"}
            return {"ok": True, "deployment": deployment, **self.validate(deployment)}
        return {"ok": False, "error": f"unknown command {command!r}"}


def serve(sock_path: Path, service: Service, ready_fd: int | None = None) -> None:
    if sock_path.exists():
        sock_path.unlink()
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(sock_path))
    server.listen(16)
    if ready_fd is not None:
        os.write(ready_fd, b"ready\n")
        os.close(ready_fd)

    def client(conn: socket.socket) -> None:
        with conn, conn.makefile("rw") as stream:
            for line in stream:
                line = line.strip()
                if not line:
                    continue
                try:
                    request = json.loads(line)
                except json.JSONDecodeError as exc:
                    response = {"ok": False, "error": f"malformed request: {exc}"}
                else:
                    response = service.handle(request)
                stream.write(json.dumps(response) + "\n")
                stream.flush()

    while True:
        conn, _ = server.accept()
        threading.Thread(target=client, args=(conn,), daemon=True).start()


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: service.py <config-dir> <socket-path>", file=sys.stderr)
        print("  the live deployment is read from stdin, never from argv", file=sys.stderr)
        return 2
    config_dir, sock_path = Path(sys.argv[1]), Path(sys.argv[2])

    # stdin carries the hidden state. Reading it and closing it means the value
    # exists only in this process's memory from here on.
    active = sys.stdin.readline().strip()
    sys.stdin.close()

    try:
        service = Service(config_dir, active)
    except ValueError as exc:
        print(f"service: {exc}", file=sys.stderr)
        return 2

    ready_fd = 4 if os.environ.get("WORLD_READY_FD") == "4" else None
    serve(sock_path, service, ready_fd)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
