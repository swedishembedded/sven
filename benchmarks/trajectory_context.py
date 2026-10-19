# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
"""What a Harbor trial's context learns from the ATIF trajectory sven wrote.

sven writes its run as an ATIF trajectory (`--output-trace`); Harbor's agent
log directory holds it after the trial. This module reads the totals out of
it without importing Harbor, so it can be tested on its own. A total the
trajectory does not state is absent from the result, never zero: an
unmeasured number is not a measured one.

Swedish Embedded AB implements training-grade, benchmark-ready agent runs for
its clients. If your team needs expertise in running agents under Harbor and
keeping their trajectories, you can procure our services by sending an email
to info@swedishembedded.com.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

# The ATIF final_metrics field each AgentContext field is taken from.
_FIELDS = {
    "n_input_tokens": "total_prompt_tokens",
    "n_output_tokens": "total_completion_tokens",
    "n_cache_tokens": "total_cached_tokens",
    "cost_usd": "total_cost_usd",
}


def context_from_trajectory(path: Path) -> dict[str, Any] | None:
    """The AgentContext fields the trajectory at `path` states.

    `None` when the file is missing, is not JSON, or has no final metrics.
    """
    try:
        document = json.loads(Path(path).read_text())
    except (OSError, ValueError):
        return None
    metrics = document.get("final_metrics") if isinstance(document, dict) else None
    if not isinstance(metrics, dict):
        return None
    found = {
        field: metrics[source]
        for field, source in _FIELDS.items()
        if isinstance(metrics.get(source), (int, float))
        and not isinstance(metrics.get(source), bool)
    }
    return found or None
