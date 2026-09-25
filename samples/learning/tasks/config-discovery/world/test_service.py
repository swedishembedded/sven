#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
"""L1 - the world, driven through its own interface. No agent, no model.

A flaky or wrong world does not announce itself: it shows up later as "the
model regressed", which is the most expensive way to find a bug in a
simulator. So every rule the task depends on is pinned here, including the
ones that feel too obvious to test - the overlay direction, the fact that
`status` is the only source of the live deployment, and that a config with an
unknown key is rejected rather than quietly accepted.

Run: python3 -m unittest discover -s world -p 'test_*.py'
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from service import DEPLOYMENTS, RETRY_KEY, Service  # noqa: E402

WORKSPACE = Path(__file__).resolve().parent.parent / "workspace"


def workspace_copy(tmp: Path) -> Path:
    subprocess.run(["cp", "-r", str(WORKSPACE), str(tmp / "ws")], check=True)
    return tmp / "ws"


class Overlay(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.ws = workspace_copy(Path(self.tmp.name))
        self.svc = Service(self.ws / "config", "staging")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_a_deployment_key_wins_over_the_base(self) -> None:
        self.assertEqual(
            self.svc.effective("staging")["endpoint"],
            "https://uploads-staging.internal/v1",
        )

    def test_a_key_absent_from_the_deployment_falls_back_to_the_base(self) -> None:
        # staging overrides only the endpoint, so the timeout is the base's.
        self.assertEqual(self.svc.effective("staging")["timeout_s"], 30)

    def test_the_base_reaches_every_deployment_that_does_not_override_it(self) -> None:
        # This is what makes "do not change development" a real constraint
        # rather than a formality: editing base.json changes development too.
        config = json.loads((self.ws / "config" / "base.json").read_text())
        config[RETRY_KEY] = 3
        (self.ws / "config" / "base.json").write_text(json.dumps(config))
        for deployment in DEPLOYMENTS:
            self.assertEqual(
                self.svc.effective(deployment).get(RETRY_KEY),
                3,
                f"editing the base must reach {deployment}",
            )


class HiddenState(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.ws = workspace_copy(Path(self.tmp.name))

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_status_is_the_only_thing_that_reports_the_live_deployment(self) -> None:
        for active in DEPLOYMENTS:
            svc = Service(self.ws / "config", active)
            self.assertEqual(svc.handle({"command": "status"})["active_deployment"], active)

    def test_no_file_in_the_workspace_binds_a_deployment_to_being_live(self) -> None:
        # Every deployment NAME appears in the workspace -- there is a config
        # file for each -- and `svctl` prints the label "active deployment:"
        # because that is what it is for. Neither is a leak. What must not
        # exist is a BINDING between one specific name and liveness, because
        # that is the fact the agent is supposed to have to ask for.
        files = {
            p.relative_to(self.ws): p.read_text(errors="ignore")
            for p in self.ws.rglob("*")
            if p.is_file()
        }
        bindings = [
            f"{marker}{sep}{deployment}"
            for deployment in DEPLOYMENTS
            for marker in ("active deployment", "active_deployment", "currently running", "live deployment")
            for sep in (": ", ":", " is ", "=", '="', "': '")
        ]
        for path, text in files.items():
            lowered = text.lower()
            for binding in bindings:
                self.assertNotIn(
                    binding.lower(),
                    lowered,
                    f"{path} states which deployment is live ({binding!r}); the agent must "
                    f"have to ask the service instead",
                )

    def test_an_unknown_deployment_is_refused_rather_than_defaulted(self) -> None:
        with self.assertRaises(ValueError):
            Service(self.ws / "config", "qa")


class Validation(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.ws = workspace_copy(Path(self.tmp.name))
        self.svc = Service(self.ws / "config", "staging")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def write(self, deployment: str, config: dict) -> None:
        (self.ws / "config" / f"{deployment}.json").write_text(json.dumps(config))

    def test_the_shipped_configuration_is_valid(self) -> None:
        # The starting state must be valid, or "make it validate" is the task
        # instead of "enable retries".
        for deployment in DEPLOYMENTS:
            self.assertTrue(self.svc.validate(deployment)["ok"], deployment)

    def test_the_obvious_spelling_is_rejected_and_names_the_real_one(self) -> None:
        # The whole point of the validate step: `retries` is guessable and
        # wrong, and the service is the only thing that says so.
        self.write("staging", {"endpoint": "https://x/v1", "retries": 3})
        result = self.svc.validate("staging")
        self.assertFalse(result["ok"])
        self.assertTrue(
            any(RETRY_KEY in p for p in result["problems"]),
            f"validation must name the accepted key: {result['problems']}",
        )

    def test_every_problem_is_reported_not_only_the_first(self) -> None:
        self.write("staging", {"retries": 3, "nonsense": 1})
        result = self.svc.validate("staging")
        self.assertGreaterEqual(len(result["problems"]), 2, result["problems"])

    def test_a_negative_or_non_integer_retry_count_is_rejected(self) -> None:
        for bad in (-1, "3", 1.5, True):
            self.write("staging", {RETRY_KEY: bad})
            self.assertFalse(self.svc.validate("staging")["ok"], f"{bad!r} must be rejected")

    def test_the_correct_edit_validates(self) -> None:
        self.write("staging", {"endpoint": "https://uploads-staging.internal/v1", RETRY_KEY: 3})
        self.assertTrue(self.svc.validate("staging")["ok"])


class Determinism(unittest.TestCase):
    def test_the_same_inputs_give_the_same_answers_twice(self) -> None:
        # A world that answers differently on a second run turns a model
        # comparison into noise.
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            first = Service(workspace_copy(Path(a)) / "config", "production")
            second = Service(workspace_copy(Path(b)) / "config", "production")
            for command in ({"command": "status"}, {"command": "effective"}, {"command": "validate"}):
                self.assertEqual(first.handle(dict(command)), second.handle(dict(command)), command)


if __name__ == "__main__":
    unittest.main()
