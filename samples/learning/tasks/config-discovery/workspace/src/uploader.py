#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
"""The upload client.

Reads the effective configuration for a deployment and uploads an artefact.
The retry behaviour is whatever the configuration says; this file has no
opinion about it and needs no change to support retries.
"""

import json
import sys
import time
from pathlib import Path

CONFIG_DIR = Path(__file__).resolve().parent.parent / "config"


def effective(deployment: str) -> dict:
    merged = json.loads((CONFIG_DIR / "base.json").read_text())
    merged.update(json.loads((CONFIG_DIR / f"{deployment}.json").read_text()))
    return merged


def upload(artefact: Path, deployment: str) -> int:
    config = effective(deployment)
    attempts = int(config.get("retry_attempts", 0)) + 1
    for attempt in range(1, attempts + 1):
        print(f"uploading {artefact.name} to {config['endpoint']} (attempt {attempt}/{attempts})")
        if send(artefact, config):
            return 0
        if attempt < attempts:
            time.sleep(0)
    print(f"upload failed after {attempts} attempt(s)", file=sys.stderr)
    return 1


def send(artefact: Path, config: dict) -> bool:
    """Placeholder transport. The artefact store is not reachable from here."""
    del artefact, config
    return False


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: uploader.py <artefact> <deployment>", file=sys.stderr)
        return 2
    return upload(Path(argv[0]), argv[1])


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
