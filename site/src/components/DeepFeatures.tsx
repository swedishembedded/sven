import FeatureSection from './FeatureSection'

export function TUIFeature() {
  return (
    <FeatureSection
      tag="Interactive TUI"
      heading="Stay in the zone."
      body={
        <>
          Sven's keyboard-driven TUI keeps your hands on the keyboard and your mind on the problem.
          Three modes — <strong className="text-text-primary">research</strong>,{' '}
          <strong className="text-text-primary">plan</strong>, and{' '}
          <strong className="text-text-primary">agent</strong> — let you dial in exactly how much
          autonomy sven has. Watch it work live, steer mid-task, or let it run unattended while you
          think.
        </>
      }
      bullets={[
        { text: 'Vim-style navigation: j/k to scroll, / to search, q to quit' },
        { text: 'Cycle modes live with F4 — from read-only research to full agent access' },
        { text: 'Streaming output: tool calls, thinking, and responses appear as they happen' },
        { text: 'Session resume with --resume: pick up any conversation right where you left off' },
        { text: 'Skills system: encode domain knowledge as files, loaded on demand' },
        { text: 'Persistent memory: sven remembers project-specific facts across sessions' },
      ]}
      imageSrc="/sven-tui.svg"
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
      heading="Debug hardware without switching tools."
      body={
        <>
          Sven is the first AI coding agent with{' '}
          <strong className="text-text-primary">native GDB integration</strong>. You wrote the
          firmware in sven — now debug it there too. Sven auto-detects your configuration from{' '}
          <span className="code-inline">.gdbinit</span>,{' '}
          <span className="code-inline">launch.json</span>, or{' '}
          <span className="code-inline">openocd.cfg</span>, connects to your device, and diagnoses
          issues autonomously — no switching to a separate debugger, no context lost.
        </>
      }
      bullets={[
        { text: 'Full GDB lifecycle: start server, connect, run, interrupt, stop — all from sven' },
        { text: 'Auto-detects config from .gdbinit, .vscode/launch.json, openocd.cfg, CMakeLists.txt' },
        { text: 'Supports JLink, OpenOCD, pyOCD, and any GDB-compatible server' },
        { text: 'Set breakpoints, inspect variables, read registers and memory in a conversation' },
        { text: 'Hardware-in-the-loop testing: run a test suite against real hardware, get a report' },
        { text: 'Understands Zephyr, FreeRTOS, and bare-metal firmware equally well' },
      ]}
      imageSrc="/sven-gdb.svg"
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
      heading="Delegate and conquer."
      body={
        <>
          Run <span className="code-inline">sven node start</span> and your agent joins a
          peer-to-peer network. Large tasks that would overwhelm one context window get split across
          specialists — a backend agent, a frontend agent, an embedded agent — each working in
          parallel. Agents discover each other via <strong className="text-text-primary">mDNS</strong>{' '}
          on your LAN or connect across the internet through a relay. No central server. No config.
        </>
      }
      bullets={[
        { text: 'mDNS auto-discovery: agents find each other on the same network automatically' },
        { text: 'Internet relay: connect agents across networks without port-forwarding' },
        { text: 'Task delegation: send a subtask to a peer and wait for the result — one tool call' },
        { text: 'Persistent rooms: agents post to named rooms for async coordination' },
        { text: 'WebAuthn passkeys: secure device authorization for the web terminal' },
        { text: 'mTLS transport: every peer connection is mutually authenticated and encrypted' },
      ]}
      imageSrc="/sven-p2p.svg"
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
      heading="Same session, same binary, now in your pipeline."
      body={
        <>
          What you prototype interactively runs{' '}
          <strong className="text-text-primary">identically in CI</strong> — no rewriting, no
          adapting, no second tool to maintain. Markdown workflow files are the source of truth for
          both local and automated runs. Sven auto-detects GitHub Actions, GitLab CI, CircleCI, and
          more — and adapts its output format accordingly.
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
      imageSrc="/sven-ci.svg"
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
