#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
"""L4 - proof that this task is solvable through the interface the agent has.

The witness is held to the agent's information boundary exactly: it may read
the workspace and it may run `svctl`, and it learns which deployment is live
the only way anyone can, by asking. It is given the socket path because the
agent is given it too (the environment sets it); it is given nothing else.

That constraint is the entire value of this file. A reference patch written by
someone who already knew the live deployment proves a solution exists. It does
not prove the solution is *discoverable*, and an undiscoverable task teaches a
model only that the task is impossible.

The witness deliberately solves the task the plodding way - ask, edit, check,
fix, check - because its tool-call count is what sets the task's budget. A
clever solver would set a budget no learner could meet.

It is not training data. Nothing here is shown to a model.
"""

import json
import os
import re
import subprocess
import sys
from pathlib import Path

REQUIRED_ATTEMPTS = 3


def svctl(workspace: Path, *args: str) -> tuple[int, str]:
    """Run the workspace's own CLI, exactly as the agent would."""
    proc = subprocess.run(
        [sys.executable, str(workspace / "svctl"), *args],
        capture_output=True,
        text=True,
        env={**os.environ},
    )
    return proc.returncode, proc.stdout + proc.stderr


def solve(workspace: Path) -> dict:
    """Returns a record of what was done, for the audit's budget accounting."""
    calls = 0

    # 1. Which deployment is live? Not in any file; ask.
    calls += 1
    rc, out = svctl(workspace, "status")
    if rc != 0:
        raise SystemExit(f"witness: could not reach the service: {out}")
    active = out.split("active deployment:")[1].strip().splitlines()[0]

    # 2. Edit that deployment's own file. Not base.json: the base reaches
    #    every deployment that does not override the key, which would change
    #    what the others do.
    target = workspace / "config" / f"{active}.json"
    config = json.loads(target.read_text())

    # 3. Use the obvious spelling first, the way someone who had not yet read
    #    the validator's output would. The point is to reach the correction.
    config["retries"] = REQUIRED_ATTEMPTS
    target.write_text(json.dumps(config, indent=2, sort_keys=True) + "\n")

    calls += 1
    rc, out = svctl(workspace, "validate")
    if rc == 0:
        raise SystemExit(
            "witness: the guessable key was accepted. The validate step is then "
            "decorative and the task has lost its second stage."
        )

    # 4. The validator names the key it accepts. Take it from there rather
    #    than from knowledge the agent would not have.
    accepted = None
    for line in out.splitlines():
        match = re.search(r"spells it ['\"]([A-Za-z_][A-Za-z0-9_]*)['\"]", line)
        if match:
            accepted = match.group(1)
            break
    if accepted is None:
        raise SystemExit(f"witness: validation did not name the accepted key:\n{out}")

    del config["retries"]
    config[accepted] = REQUIRED_ATTEMPTS
    target.write_text(json.dumps(config, indent=2, sort_keys=True) + "\n")

    calls += 1
    rc, out = svctl(workspace, "validate")
    if rc != 0:
        raise SystemExit(f"witness: the corrected configuration still does not validate:\n{out}")

    return {"active_deployment": active, "accepted_key": accepted, "tool_calls": calls}


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: solve.py <workspace>", file=sys.stderr)
        return 2
    record = solve(Path(sys.argv[1]).resolve())
    print(json.dumps(record, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
