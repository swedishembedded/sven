#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
"""L2 - decide whether the request was carried out.

Runs after the episode, from outside the workspace, against the environment's
own view of the world. It never reads the agent's account of what it did: the
only inputs are the final configuration files and what the service says about
them.

Every predicate here traces to a phrase in the task's request or to behaviour
the workspace documents. That is not a style rule - a predicate that traces to
neither is a requirement the agent was never given, and training on it teaches
guessing.

Emits one JSON object mapping predicate name to a boolean, plus the evidence
behind each, so a failure says which part of the request went unmet.
"""

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from service import DEPLOYMENTS, RETRY_KEY, Service  # noqa: E402

#: How many retry attempts the request asks for.
REQUIRED_ATTEMPTS = 3


def effective_all(config_dir: Path, active: str) -> dict:
    svc = Service(config_dir, active)
    out = {}
    for deployment in DEPLOYMENTS:
        try:
            out[deployment] = svc.effective(deployment)
        except (FileNotFoundError, json.JSONDecodeError) as exc:
            out[deployment] = {"__unreadable__": str(exc)}
    return out


def verify(config_dir: Path, active: str, baseline: dict) -> dict:
    svc = Service(config_dir, active)
    final = effective_all(config_dir, active)
    evidence: dict = {"active_deployment": active, "final_effective": final}

    # "Enable 3 retry attempts for the upload service this host is running."
    live = final.get(active, {})
    retries_enabled = live.get(RETRY_KEY) == REQUIRED_ATTEMPTS

    # The service rejects configurations it does not fully understand
    # (docs/config.md). A change the service will not accept has not been made.
    validation = svc.validate(active)
    evidence["validation"] = validation

    # "Do not change what any other deployment does." Compared on EFFECTIVE
    # configuration, not on file contents: the request is about behaviour, and
    # a reformatted file that resolves identically has changed nothing. It
    # also catches the trap in the other direction -- editing base.json leaves
    # the other deployment files untouched while changing what they do.
    others_unchanged = True
    changed = {}
    for deployment in DEPLOYMENTS:
        if deployment == active:
            continue
        if final.get(deployment) != baseline.get(deployment):
            others_unchanged = False
            changed[deployment] = {"before": baseline.get(deployment), "after": final.get(deployment)}
    evidence["changed_other_deployments"] = changed

    return {
        "predicates": {
            "active_deployment_retries_enabled": bool(retries_enabled),
            "active_deployment_validates": bool(validation["ok"]),
            "other_deployments_unchanged": bool(others_unchanged),
        },
        "evidence": evidence,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--config-dir", required=True, type=Path)
    ap.add_argument("--active", required=True, choices=DEPLOYMENTS)
    ap.add_argument(
        "--baseline",
        required=True,
        type=Path,
        help="effective configuration of every deployment BEFORE the episode",
    )
    ap.add_argument("--out", type=Path)
    args = ap.parse_args()

    baseline = json.loads(args.baseline.read_text())
    result = verify(args.config_dir, args.active, baseline)
    text = json.dumps(result, indent=2, sort_keys=True)
    if args.out:
        args.out.write_text(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
