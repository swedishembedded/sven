# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
"""Spec: a Harbor trial's token and cost totals come from the ATIF trajectory
sven wrote, and a total it does not state is absent, never zero."""

import json
import tempfile
import unittest
from pathlib import Path

from trajectory_context import context_from_trajectory


def write(directory: str, text: str) -> Path:
    path = Path(directory) / "trajectory.json"
    path.write_text(text)
    return path


class ContextFromTrajectory(unittest.TestCase):
    def test_the_totals_the_trajectory_states_are_taken(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = write(
                tmp,
                json.dumps(
                    {
                        "schema_version": "ATIF-v1.7",
                        "final_metrics": {
                            "total_prompt_tokens": 1200,
                            "total_completion_tokens": 340,
                            "total_cached_tokens": 800,
                            "total_cost_usd": 0.5,
                        },
                    }
                ),
            )
            self.assertEqual(
                context_from_trajectory(path),
                {
                    "n_input_tokens": 1200,
                    "n_output_tokens": 340,
                    "n_cache_tokens": 800,
                    "cost_usd": 0.5,
                },
            )

    def test_a_total_that_is_not_stated_is_absent_not_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = write(
                tmp, json.dumps({"final_metrics": {"total_prompt_tokens": 7}})
            )
            self.assertEqual(context_from_trajectory(path), {"n_input_tokens": 7})

    def test_a_trajectory_without_metrics_or_a_file_that_is_not_json_gives_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(context_from_trajectory(write(tmp, "{}")))
            self.assertIsNone(context_from_trajectory(write(tmp, "not json")))
            self.assertIsNone(context_from_trajectory(Path(tmp) / "missing.json"))
            self.assertIsNone(
                context_from_trajectory(
                    write(tmp, json.dumps({"final_metrics": {"total_cost_usd": True}}))
                )
            )


if __name__ == "__main__":
    unittest.main()
