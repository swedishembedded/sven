"""Harbor installed-agent adapter for Sven.

Wraps the `sven` CLI binary as a Harbor BaseInstalledAgent so it can be
evaluated against Terminal-Bench 2.0 (and any other Harbor-compatible dataset)
via:

    harbor run -d terminal-bench@2.0 \\
        --agent-import-path benchmarks.sven_agent:SvenInstalledAgent \\
        -o target/benchmark/terminal-bench

The binary is installed inside each sandbox by install_sven.sh.j2, which
copies the pre-built release binary from a bind-mount path on the host.

Environment variables (read at run time, forwarded into each sandbox):

    SVEN_MODEL            – model string passed to sven --model
                            (default: openrouter/openrouter/free)
    SVEN_BIN_PATH         – host path to the pre-built sven binary; the
                            install template copies it into the container
                            (default: /sven-bin/sven)
    SVEN_BENCH_TIMEOUT    – per-task timeout in seconds (default: 1800)
    ANTHROPIC_API_KEY     – forwarded verbatim into the sandbox
    OPENAI_API_KEY        – forwarded verbatim into the sandbox
    OPENROUTER_API_KEY    – forwarded verbatim into the sandbox
    GEMINI_API_KEY        – forwarded verbatim into the sandbox
"""

from __future__ import annotations

import os
import shlex
from pathlib import Path

from harbor.agents.installed.base import BaseInstalledAgent, ExecInput
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

_API_KEY_VARS = (
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GEMINI_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "GROQ_API_KEY",
    "MISTRAL_API_KEY",
)


class SvenInstalledAgent(BaseInstalledAgent):
    """Harbor agent that installs and invokes the `sven` CLI inside the sandbox.

    The install template (install_sven.sh.j2) copies a pre-built release
    binary from a host path that is bind-mounted into the container by Harbor.
    """

    @staticmethod
    def name() -> str:
        return "sven"

    def version(self) -> str | None:
        return self._version

    # ------------------------------------------------------------------
    # Install template
    # ------------------------------------------------------------------

    @property
    def _install_agent_template_path(self) -> Path:
        return Path(__file__).parent / "install_sven.sh.j2"

    # The binary is uploaded to this fixed path inside the container by setup().
    _CONTAINER_BIN_PATH = "/installed-agent/sven"

    @property
    def _template_variables(self) -> dict[str, str]:
        return {
            "sven_bin_path": self._CONTAINER_BIN_PATH,
        }

    async def setup(self, environment: BaseEnvironment) -> None:
        host_bin = Path(os.getenv("SVEN_BIN_PATH", "target/release/sven"))
        if not host_bin.is_absolute():
            host_bin = Path.cwd() / host_bin
        if not host_bin.exists():
            raise FileNotFoundError(
                f"sven binary not found at {host_bin}. "
                "Run 'make release' first."
            )
        # Upload binary before the base setup() renders and executes the
        # install template, so /installed-agent/sven exists when the script runs.
        await environment.exec(command="mkdir -p /installed-agent")
        await environment.upload_file(
            source_path=host_bin,
            target_path=self._CONTAINER_BIN_PATH,
        )
        await super().setup(environment)

    # ------------------------------------------------------------------
    # Run commands
    # ------------------------------------------------------------------

    def create_run_agent_commands(self, instruction: str) -> list[ExecInput]:
        model = os.getenv("SVEN_MODEL", "openrouter/openrouter/free")
        timeout = int(os.getenv("SVEN_BENCH_TIMEOUT", "1800"))

        env: dict[str, str] = {"SVEN_MODEL": model}
        for key in _API_KEY_VARS:
            value = os.getenv(key)
            if value:
                env[key] = value

        return [
            ExecInput(
                command=(
                    f"sven --headless --model {shlex.quote(model)} --output-format compact"
                    f" {shlex.quote(instruction)}"
                ),
                # cwd=None → use the container's WORKDIR from its Dockerfile.
                # Each Terminal-Bench task sets its own working directory;
                # hardcoding a path here breaks tasks that use /app, /root, etc.
                timeout_sec=timeout,
                env=env,
            )
        ]

    # ------------------------------------------------------------------
    # Post-run context collection
    # ------------------------------------------------------------------

    def populate_context_post_run(self, context: AgentContext) -> None:
        # Harbor captures stdout/stderr automatically from the container.
        # Sven writes its final answer to stdout and diagnostics to stderr,
        # which matches what Harbor expects from a headless installed agent.
        #
        # For richer trajectory capture, add --output-jsonl to the run command
        # above, download that file here, parse each JSON line into ATIF Steps,
        # build a Trajectory, and set context.n_input_tokens / context.cost_usd.
        pass
