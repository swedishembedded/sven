#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
"""The offline audit for this family: L1-L5, no model, no GPU, no network.

Each check answers a question that has to be settled before a model is
involved, because a model result is uninterpretable until it is:

  L5  does the untouched workspace FAIL?           else there is nothing to learn
  L4  does an observation-only witness SOLVE it?   else the task is not discoverable
  L2  does the verifier reject known-bad work?     else "solved" means nothing
  L3  is the answer absent from the workspace?     else it is a reading exercise

Exit status is the gate: 0 only when every check passes, so this can be wired
into a build without reading its output.

Usage: python3 world/audit.py [--keep]
"""

import argparse
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

FAMILY = Path(__file__).resolve().parent.parent
WORKSPACE = FAMILY / "workspace"
WORLD = FAMILY / "world"
WITNESS = FAMILY / "witness" / "solve.py"

sys.path.insert(0, str(WORLD))
from service import DEPLOYMENTS, RETRY_KEY  # noqa: E402
from verify import REQUIRED_ATTEMPTS, effective_all, verify  # noqa: E402

#: Deployments an instance may draw as live. `development` is excluded on
#: purpose: the request tells the agent not to change what the other
#: deployments do, and a live `development` would make the request
#: self-contradictory rather than merely hard.
LIVE_CHOICES = ("staging", "production")


class World:
    """The service, started the way the lab starts it: state over a pipe."""

    def __init__(self, workspace: Path, active: str):
        self.workspace = workspace
        self.active = active
        self.sock = workspace.parent / "service.sock"
        read_fd, write_fd = os.pipe()
        os.write(write_fd, f"{active}\n".encode())
        os.close(write_fd)
        self.proc = subprocess.Popen(
            [sys.executable, str(WORLD / "service.py"), str(workspace / "config"), str(self.sock)],
            pass_fds=(read_fd,),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        os.close(read_fd)
        self._await_socket()

    def _await_socket(self, timeout: float = 10.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.sock.exists():
                try:
                    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    s.connect(str(self.sock))
                    s.close()
                    return
                except OSError:
                    pass
            if self.proc.poll() is not None:
                raise SystemExit(f"world died at startup: {self.proc.stderr.read().decode()}")
            time.sleep(0.05)
        raise SystemExit("world did not accept connections in time")

    def env(self) -> dict:
        return {**os.environ, "UPLOAD_SERVICE_SOCKET": str(self.sock)}

    def stop(self) -> None:
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def materialise(into: Path) -> Path:
    ws = into / "workspace"
    shutil.copytree(WORKSPACE, ws)
    return ws


def run_verifier(ws: Path, active: str, baseline: dict) -> dict:
    return verify(ws / "config", active, baseline)


def check(name: str, ok: bool, detail: str = "") -> bool:
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}{(': ' + detail) if detail else ''}")
    return ok


def audit_instance(active: str, keep: bool) -> bool:
    print(f"\n== instance: live deployment is {active!r}")
    tmp = Path(tempfile.mkdtemp(prefix=f"audit-{active}-"))
    ok = True
    try:
        ws = materialise(tmp)
        world = World(ws, active)
        env = world.env()
        baseline = effective_all(ws / "config", active)

        # L5 - the untouched workspace must fail. An instance the starting
        # state already satisfies teaches nothing and inflates every score.
        before = run_verifier(ws, active, baseline)
        ok &= check(
            "L5 untouched workspace fails the request",
            not all(before["predicates"].values()),
            f"unmet: {[k for k, v in before['predicates'].items() if not v]}",
        )

        # L3 (static half) - the answer must not be in the workspace. The live
        # deployment is the answer here, so no file may bind one to liveness.
        blob = "\n".join(
            p.read_text(errors="ignore") for p in ws.rglob("*") if p.is_file()
        ).lower()
        leaked = [
            f"{m}{s}{active}"
            for m in ("active deployment", "active_deployment", "live deployment", "currently running")
            for s in (": ", ":", " is ", "=")
            if f"{m}{s}{active}".lower() in blob
        ]
        ok &= check("L3 the workspace does not name the live deployment", not leaked, str(leaked))

        # L4 - an observation-only witness must solve it, through svctl only.
        proc = subprocess.run(
            [sys.executable, str(WITNESS), str(ws)],
            capture_output=True, text=True, env=env,
        )
        solved_by_witness = proc.returncode == 0
        ok &= check("L4 an observation-only witness solves it", solved_by_witness,
                    proc.stderr.strip()[:200])
        budget = 0
        if solved_by_witness:
            record = json.loads(proc.stdout)
            budget = record["tool_calls"]
            ok &= check(
                "L4 the witness discovered the live deployment rather than assuming it",
                record["active_deployment"] == active,
                f"witness said {record['active_deployment']!r}",
            )
            after = run_verifier(ws, active, baseline)
            ok &= check(
                "L2 the verifier accepts the witness's work",
                all(after["predicates"].values()),
                f"unmet: {[k for k, v in after['predicates'].items() if not v]}",
            )
            print(f"         witness used {budget} tool call(s)")

        world.stop()
        return ok
    finally:
        if keep:
            print(f"         kept: {tmp}")
        else:
            shutil.rmtree(tmp, ignore_errors=True)


def audit_negatives(keep: bool) -> bool:
    """L2 - every known-bad solution must be rejected, for the right reason."""
    print("\n== negatives: the verifier must reject each of these")
    cases = [
        (
            "edits base.json, which changes what the other deployments do",
            lambda ws, active: patch(ws / "config" / "base.json", {RETRY_KEY: REQUIRED_ATTEMPTS}),
            "other_deployments_unchanged",
        ),
        (
            "edits the wrong deployment's file",
            lambda ws, active: patch(
                ws / "config" / f"{[d for d in LIVE_CHOICES if d != active][0]}.json",
                {RETRY_KEY: REQUIRED_ATTEMPTS},
            ),
            "active_deployment_retries_enabled",
        ),
        (
            "uses the guessable key spelling",
            lambda ws, active: patch(ws / "config" / f"{active}.json", {"retries": REQUIRED_ATTEMPTS}),
            "active_deployment_validates",
        ),
        (
            "sets the wrong number of attempts",
            lambda ws, active: patch(ws / "config" / f"{active}.json", {RETRY_KEY: 1}),
            "active_deployment_retries_enabled",
        ),
        (
            "changes nothing at all",
            lambda ws, active: None,
            "active_deployment_retries_enabled",
        ),
    ]

    ok = True
    for description, mutate, expected_failure in cases:
        tmp = Path(tempfile.mkdtemp(prefix="audit-neg-"))
        try:
            ws = materialise(tmp)
            active = "staging"
            world = World(ws, active)
            baseline = effective_all(ws / "config", active)
            mutate(ws, active)
            result = run_verifier(ws, active, baseline)
            failed = [k for k, v in result["predicates"].items() if not v]
            # Rejected is not enough: it has to be rejected for the stated
            # reason. A verifier that fails everything for one reason would
            # pass a "was it rejected?" check while measuring nothing.
            ok &= check(
                f"rejects: {description}",
                expected_failure in failed,
                f"expected {expected_failure!r} to fail, got {failed}",
            )
            world.stop()
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
    return ok


def patch(path: Path, updates: dict) -> None:
    config = json.loads(path.read_text())
    config.update(updates)
    path.write_text(json.dumps(config, indent=2, sort_keys=True) + "\n")


def audit_paired_worlds() -> bool:
    """L3 - the twins must be indistinguishable without asking the service."""
    print("\n== paired worlds: identical workspace, different required outcome")
    digests = {}
    for active in LIVE_CHOICES:
        tmp = Path(tempfile.mkdtemp(prefix="audit-pair-"))
        try:
            ws = materialise(tmp)
            h = hashlib.sha256()
            for p in sorted(ws.rglob("*")):
                if p.is_file():
                    h.update(str(p.relative_to(ws)).encode())
                    h.update(p.read_bytes())
            digests[active] = h.hexdigest()
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
    same = len(set(digests.values())) == 1
    return check(
        "L3 the twins' workspaces are byte-identical",
        same,
        "a static solver could tell them apart" if not same else f"sha256 {list(digests.values())[0][:16]}",
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--keep", action="store_true", help="keep the materialised workspaces")
    args = ap.parse_args()

    print("audit: config-discovery")
    ok = audit_paired_worlds()
    for active in LIVE_CHOICES:
        ok &= audit_instance(active, args.keep)
    ok &= audit_negatives(args.keep)

    print()
    if ok:
        print("audit: OK - every check passed")
        return 0
    print("audit: FAILED - see the checks above")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
