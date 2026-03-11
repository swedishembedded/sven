#!/usr/bin/env python3
"""Generate a Markdown benchmark report from Harbor job output directories.

Usage:
    python3 benchmarks/report.py target/benchmark > target/benchmark/report.md
    python3 benchmarks/report.py target/benchmark/terminal-bench

Harbor stores one job per subdirectory.  Each job contains per-trial
subdirectories, each with a ``result.json`` file (a serialised TrialResult).

Structure read:
    <jobs-dir>/
        <job-name>/
            result.json          – JobResult (summary, optional)
            <trial-name>/
                result.json      – TrialResult per task

Output (stdout):
    A Markdown document suitable for committing or displaying in CI.
"""

from __future__ import annotations

import json
import sys
from datetime import datetime, timezone
from pathlib import Path
from statistics import median
from typing import Any


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _seconds(started: str | None, finished: str | None) -> float | None:
    """Return wall-clock seconds between two ISO-8601 timestamps, or None."""
    if not started or not finished:
        return None
    try:
        fmt = "%Y-%m-%dT%H:%M:%S.%f%z"
        # Python's fromisoformat handles most ISO-8601 variants in 3.11+
        t0 = datetime.fromisoformat(started)
        t1 = datetime.fromisoformat(finished)
        delta = (t1 - t0).total_seconds()
        return delta if delta >= 0 else None
    except (ValueError, TypeError):
        return None


def _fmt_seconds(s: float) -> str:
    m, sec = divmod(int(s), 60)
    return f"{m}m {sec:02d}s"


def _reward_passed(rewards: dict[str, Any] | None) -> bool:
    """Return True if the task verifier considers this trial a pass.

    Harbor's Terminal-Bench verifier writes ``1`` (pass) or ``0`` (fail) to
    ``reward.txt``.  The ``VerifierResult.rewards`` dict can have arbitrary
    keys; the canonical key is ``"score"`` for Terminal-Bench.  A trial is
    considered passed when *any* numeric reward value rounds to 1.
    """
    if not rewards:
        return False
    for v in rewards.values():
        try:
            if float(v) >= 1.0:
                return True
        except (TypeError, ValueError):
            pass
    return False


def _category_from_task(task_name: str) -> str:
    """Derive a display category label from the task name.

    Terminal-Bench task names follow the pattern ``<category>__<slug>``
    (double underscore separator).  If no separator is present, return the
    first path component or ``"unknown"``.
    """
    if "__" in task_name:
        return task_name.split("__", 1)[0]
    if "/" in task_name:
        return task_name.split("/")[0]
    return "unknown"


# ---------------------------------------------------------------------------
# Data collection
# ---------------------------------------------------------------------------

def _load_trial_results(job_dir: Path) -> list[dict[str, Any]]:
    """Walk a job directory and return all TrialResult dicts found."""
    results: list[dict[str, Any]] = []
    for candidate in sorted(job_dir.rglob("result.json")):
        # Skip the top-level job summary (it lacks a task_name field).
        try:
            data = json.loads(candidate.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        if "task_name" in data:
            results.append(data)
    return results


def _collect_jobs(root: Path) -> dict[str, list[dict[str, Any]]]:
    """Return a mapping of dataset_name -> list[TrialResult dicts].

    Handles two layouts:
      1. root is a single job directory (contains trial subdirs directly).
      2. root contains multiple job directories (one per dataset run).
    """
    jobs: dict[str, list[dict[str, Any]]] = {}

    # Check whether root itself is a job directory.
    direct = _load_trial_results(root)
    if direct:
        # Filter to only immediate children (not recursing into sub-jobs).
        direct = [
            r for r in direct
            if "task_name" in r
        ]
        if direct:
            jobs[root.name] = direct
            return jobs

    # Otherwise iterate child directories as separate jobs.
    for child in sorted(root.iterdir()):
        if not child.is_dir():
            continue
        trials = _load_trial_results(child)
        if trials:
            jobs[child.name] = trials

    return jobs


# ---------------------------------------------------------------------------
# Stats
# ---------------------------------------------------------------------------

class DatasetStats:
    def __init__(self, name: str, trials: list[dict[str, Any]]) -> None:
        self.name = name
        self._trials = trials

    @property
    def total(self) -> int:
        return len(self._trials)

    @property
    def passed(self) -> int:
        return sum(
            1 for t in self._trials
            if _reward_passed(
                (t.get("verifier_result") or {}).get("rewards")
            )
        )

    @property
    def errors(self) -> int:
        return sum(1 for t in self._trials if t.get("exception_info"))

    @property
    def pass_rate(self) -> float:
        return self.passed / self.total if self.total else 0.0

    def _exec_seconds(self) -> list[float]:
        times: list[float] = []
        for t in self._trials:
            ae = t.get("agent_execution") or {}
            s = _seconds(ae.get("started_at"), ae.get("finished_at"))
            if s is not None:
                times.append(s)
        return times

    @property
    def median_time_sec(self) -> float | None:
        times = self._exec_seconds()
        return median(times) if times else None

    @property
    def total_input_tokens(self) -> int:
        total = 0
        for t in self._trials:
            ar = t.get("agent_result") or {}
            total += ar.get("n_input_tokens") or 0
        return total

    @property
    def total_output_tokens(self) -> int:
        total = 0
        for t in self._trials:
            ar = t.get("agent_result") or {}
            total += ar.get("n_output_tokens") or 0
        return total

    @property
    def total_cost_usd(self) -> float:
        total = 0.0
        for t in self._trials:
            ar = t.get("agent_result") or {}
            total += ar.get("cost_usd") or 0.0
        return total

    def by_category(self) -> dict[str, tuple[int, int]]:
        """Return {category: (passed, total)}."""
        cat: dict[str, list[int]] = {}
        for t in self._trials:
            c = _category_from_task(t.get("task_name", ""))
            passed = int(_reward_passed(
                (t.get("verifier_result") or {}).get("rewards")
            ))
            if c not in cat:
                cat[c] = [0, 0]
            cat[c][0] += passed
            cat[c][1] += 1
        return {k: (v[0], v[1]) for k, v in sorted(cat.items())}

    def slowest_tasks(self, n: int = 5) -> list[tuple[str, float]]:
        """Return the n slowest tasks as (task_name, seconds) pairs."""
        pairs: list[tuple[str, float]] = []
        for t in self._trials:
            ae = t.get("agent_execution") or {}
            s = _seconds(ae.get("started_at"), ae.get("finished_at"))
            if s is not None:
                pairs.append((t.get("task_name", "?"), s))
        return sorted(pairs, key=lambda x: -x[1])[:n]

    def composite_score(self) -> float:
        """Weighted composite score.

        score = 0.50 * pass_rate
              + 0.20 * quality_score   (same as pass_rate for now)
              + 0.15 * speed_score     (1 - median_time / timeout, clamped)
              + 0.15 * cost_score      (1 - cost_per_task / budget, clamped)

        Speed and cost components fall back to 0 when data is unavailable.
        """
        TIMEOUT_SEC = 1800.0
        COST_BUDGET_USD = 1.0  # per task

        speed = 0.0
        mt = self.median_time_sec
        if mt is not None:
            speed = max(0.0, 1.0 - mt / TIMEOUT_SEC)

        cost = 0.0
        if self.total:
            cost_per_task = self.total_cost_usd / self.total
            cost = max(0.0, 1.0 - cost_per_task / COST_BUDGET_USD)

        return (
            0.50 * self.pass_rate
            + 0.20 * self.pass_rate
            + 0.15 * speed
            + 0.15 * cost
        )


# ---------------------------------------------------------------------------
# Markdown rendering
# ---------------------------------------------------------------------------

def _render_dataset(stats: DatasetStats) -> str:
    lines: list[str] = []

    lines.append(f"## {stats.name}")
    lines.append("")
    lines.append(f"- **Tasks**: {stats.total}")
    lines.append(f"- **Passed**: {stats.passed} / {stats.total}"
                 f"  ({stats.pass_rate * 100:.1f}%)")
    lines.append(f"- **Errors**: {stats.errors}")
    lines.append(f"- **Composite score**: {stats.composite_score():.3f}")
    lines.append("")

    # Category breakdown
    by_cat = stats.by_category()
    if by_cat:
        lines.append("### By category")
        lines.append("")
        lines.append("| Category | Passed | Total | Rate |")
        lines.append("|---|---|---|---|")
        for cat, (p, t) in by_cat.items():
            rate = f"{p / t * 100:.0f}%" if t else "—"
            lines.append(f"| {cat} | {p} | {t} | {rate} |")
        lines.append("")

    # Performance
    lines.append("### Performance")
    lines.append("")
    mt = stats.median_time_sec
    lines.append(
        f"- **Median task time**: {_fmt_seconds(mt) if mt is not None else 'n/a'}"
    )
    slowest = stats.slowest_tasks(5)
    if slowest:
        lines.append("- **Slowest tasks**:")
        for task, secs in slowest:
            lines.append(f"  - `{task}` — {_fmt_seconds(secs)}")
    lines.append("")

    # Cost / tokens
    lines.append("### Cost")
    lines.append("")
    if stats.total_input_tokens or stats.total_output_tokens:
        lines.append(f"- **Input tokens**: {stats.total_input_tokens:,}")
        lines.append(f"- **Output tokens**: {stats.total_output_tokens:,}")
    if stats.total_cost_usd:
        lines.append(f"- **Estimated cost**: ${stats.total_cost_usd:.4f}")
        if stats.passed:
            lines.append(
                f"- **Cost per solved task**: "
                f"${stats.total_cost_usd / stats.passed:.4f}"
            )
    if not (stats.total_input_tokens or stats.total_output_tokens or stats.total_cost_usd):
        lines.append("- *(no token/cost data available)*")
    lines.append("")

    return "\n".join(lines)


def generate_report(root: Path) -> str:
    jobs = _collect_jobs(root)
    if not jobs:
        return (
            f"# Sven Benchmark Report\n\n"
            f"No results found under `{root}`.\n\n"
            "Run `make benchmark` to generate results.\n"
        )

    now = datetime.now(tz=timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    sections: list[str] = [f"# Sven Benchmark Report — {now}\n"]

    all_pass = 0
    all_total = 0
    for job_name, trials in jobs.items():
        stats = DatasetStats(job_name, trials)
        sections.append(_render_dataset(stats))
        all_pass += stats.passed
        all_total += stats.total

    if len(jobs) > 1:
        overall_rate = all_pass / all_total * 100 if all_total else 0.0
        sections.append(
            f"---\n\n**Overall: {all_pass} / {all_total}"
            f" ({overall_rate:.1f}%) across {len(jobs)} dataset(s)**\n"
        )

    return "\n".join(sections)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    if len(sys.argv) < 2:
        print(
            "Usage: report.py <jobs-dir>\n"
            "Example: report.py target/benchmark",
            file=sys.stderr,
        )
        sys.exit(1)

    root = Path(sys.argv[1])
    if not root.exists():
        print(f"Error: directory not found: {root}", file=sys.stderr)
        sys.exit(1)

    print(generate_report(root))


if __name__ == "__main__":
    main()
