import FeatureSection from './FeatureSection'

export function TUIFeature() {
  return (
    <FeatureSection
      tag="Interactive TUI"
      heading="Your terminal, supercharged."
      body={
        <>
          Sven gives you a keyboard-driven interface that stays out of your way. Three operating
          modes — <strong className="text-text-primary">research</strong>,{' '}
          <strong className="text-text-primary">plan</strong>, and{' '}
          <strong className="text-text-primary">agent</strong> — let you control exactly how much
          access the agent has. Watch it work in real time, steer mid-task, or let it run
          unattended.
        </>
      }
      bullets={[
        { text: 'Vim-style navigation: j/k to scroll, / to search, q to quit' },
        { text: 'Cycle modes live with F4 — from read-only research to full agent access' },
        { text: 'Streaming output: tool calls, thinking, and responses appear as they happen' },
        { text: 'Session resume with --resume: pick up any conversation right where you left off' },
        { text: 'Skills system: domain-specific instruction files loaded on demand' },
        { text: 'Persistent memory: durable facts carried across sessions via update_memory' },
      ]}
      imageSrc="/placeholder-tui.svg"
      imageAlt="Sven TUI interactive session showing chip logo, conversation, and tool calls"
      imageWidth={800}
      imageHeight={500}
    />
  )
}

export function GDBFeature() {
  return (
    <FeatureSection
      tag="Industry First · Embedded Debugging"
      tagColor="#e6b428"
      accentColor="#e6b428"
      heading="Debug hardware with an AI copilot."
      body={
        <>
          Sven is the first AI coding agent with{' '}
          <strong className="text-text-primary">native GDB integration</strong>. It auto-detects
          your debug configuration from <span className="code-inline">.gdbinit</span>,{' '}
          <span className="code-inline">launch.json</span>, or{' '}
          <span className="code-inline">openocd.cfg</span>, connects to your device, and starts
          diagnosing issues — setting breakpoints, reading registers, and inspecting memory without
          you lifting a finger.
        </>
      }
      bullets={[
        { text: 'Full GDB lifecycle: start server, connect, run, interrupt, stop — all from Sven' },
        { text: 'Auto-detects config from .gdbinit, .vscode/launch.json, openocd.cfg, CMakeLists.txt' },
        { text: 'Supports JLink, OpenOCD, pyOCD, and any GDB-compatible server' },
        { text: 'Set breakpoints, inspect variables, read registers and memory in a conversation' },
        { text: 'Hardware-in-the-loop testing: Sven can run a test suite against real hardware' },
        { text: 'Understands Zephyr, FreeRTOS, and bare-metal firmware equally well' },
      ]}
      imageSrc="/placeholder-gdb.svg"
      imageAlt="Sven GDB embedded debugging session with hardware breakpoints and memory inspection"
      imageWidth={800}
      imageHeight={500}
      reverse
    />
  )
}

export function P2PFeature() {
  return (
    <FeatureSection
      tag="Multi-Agent P2P"
      heading="Agents that work together."
      body={
        <>
          Run <span className="code-inline">sven node start</span> and your agent joins a
          peer-to-peer network. Agents discover each other via{' '}
          <strong className="text-text-primary">mDNS</strong> on your LAN or connect across the
          internet through a relay. Delegate subtasks, search peer conversations, and build
          multi-agent workflows — all secured with mTLS and WebAuthn.
        </>
      }
      bullets={[
        { text: 'mDNS auto-discovery: agents find each other on the same network automatically' },
        { text: 'Internet relay: connect agents across networks without port-forwarding' },
        { text: 'Task delegation: send a subtask to a peer and wait for the result — one line' },
        { text: 'Persistent rooms: agents can post to named rooms for async coordination' },
        { text: 'WebAuthn passkeys: secure device authorization for the web terminal' },
        { text: 'mTLS transport: every peer connection is mutually authenticated and encrypted' },
      ]}
      imageSrc="/placeholder-p2p.svg"
      imageAlt="Diagram of multiple Sven agent nodes connected in a P2P network"
      imageWidth={800}
      imageHeight={500}
    >
      <div className="flex flex-col sm:flex-row gap-3">
        <a
          href="https://github.com/swedishembedded/sven/tree/main/docs/08-node.md"
          target="_blank"
          rel="noopener noreferrer"
          className="btn-secondary text-sm"
        >
          Node documentation →
        </a>
      </div>
    </FeatureSection>
  )
}

export function CIFeature() {
  return (
    <FeatureSection
      tag="CI/CD Integration"
      accentColor="#4ade80"
      heading="From chat to pipeline in zero changes."
      body={
        <>
          Every workflow you develop interactively runs{' '}
          <strong className="text-text-primary">identically in CI</strong>. Markdown workflow files
          with steps, variables, and per-step options give you reproducible automation. Sven
          auto-detects GitHub Actions, GitLab CI, CircleCI, and more — and adapts its output
          accordingly.
        </>
      }
      bullets={[
        { text: 'Markdown workflows: ## Step headings, YAML frontmatter, {{variable}} templating' },
        { text: 'Output formats: conversation, compact, json, jsonl — pipe between agents' },
        { text: 'Auto-detects CI: GitHub Actions, GitLab, CircleCI, Travis, Jenkins, Azure, Bitbucket' },
        { text: 'Meaningful exit codes: 0 = success, non-zero = failure, always machine-readable' },
        { text: 'GitHub Actions integration: official composite action for one-line CI setup' },
        { text: 'Headless mode: --headless "task" writes to stdout and exits — no TTY needed' },
      ]}
      imageSrc="/placeholder-ci.svg"
      imageAlt="Sven workflow file running in a CI pipeline with step-by-step output"
      imageWidth={800}
      imageHeight={500}
      reverse
    >
      <div className="flex flex-col sm:flex-row gap-3">
        <a
          href="https://github.com/swedishembedded/sven/tree/main/docs/04-ci-pipeline.md"
          target="_blank"
          rel="noopener noreferrer"
          className="btn-secondary text-sm"
        >
          CI pipeline guide →
        </a>
      </div>
    </FeatureSection>
  )
}
