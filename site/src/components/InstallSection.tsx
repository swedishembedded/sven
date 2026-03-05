import { useState, useRef } from 'react'
import { motion, useInView } from 'framer-motion'

type TabId = 'oneliner' | 'deb' | 'source' | 'macos'

interface Tab {
  id: TabId
  label: string
  icon: React.ReactNode
}

const TABS: Tab[] = [
  { id: 'oneliner', label: 'One-liner', icon: <BoltIcon /> },
  { id: 'deb', label: 'Debian / Ubuntu', icon: <PackageIcon /> },
  { id: 'source', label: 'Build from Source', icon: <CodeIcon /> },
  { id: 'macos', label: 'macOS', icon: <AppleIcon /> },
]

interface TabContent {
  steps: { label?: string; code: string; comment?: string }[]
  note?: React.ReactNode
}

const TAB_CONTENT: Record<TabId, TabContent> = {
  oneliner: {
    steps: [
      {
        label: 'Install (Linux x86_64 or aarch64)',
        code: 'curl -fsSL https://agentsven.com/install | sh',
        comment: '# Installs to /usr/local/bin',
      },
      {
        label: 'Set your API key',
        code: 'export OPENAI_API_KEY="sk-..."',
        comment: '# Or ANTHROPIC_API_KEY, etc.',
      },
      {
        label: 'Run',
        code: 'sven',
        comment: '# Opens the TUI',
      },
    ],
    note: (
      <p className="text-xs text-text-secondary">
        Supports{' '}
        <code className="code-inline">SVEN_VERSION</code>,{' '}
        <code className="code-inline">SVEN_INSTALL_DIR</code>, and{' '}
        <code className="code-inline">SVEN_NO_SUDO</code> env vars for customization.
      </p>
    ),
  },
  deb: {
    steps: [
      {
        label: 'Download the latest .deb',
        code: 'wget https://github.com/swedishembedded/sven/releases/latest/download/sven-linux-x86_64.deb',
      },
      {
        label: 'Install',
        code: 'sudo dpkg -i sven-linux-x86_64.deb',
        comment: '# Shell completions installed automatically',
      },
      {
        label: 'Set your API key and run',
        code: 'export OPENAI_API_KEY="sk-..."\nsven',
      },
    ],
    note: (
      <p className="text-xs text-text-secondary">
        ARM64 package also available:{' '}
        <code className="code-inline">sven-linux-aarch64.deb</code>
      </p>
    ),
  },
  source: {
    steps: [
      {
        label: 'Install Rust (if needed)',
        code: 'curl --proto \'=https\' --tlsv1.2 -sSf https://sh.rustup.rs | sh',
      },
      {
        label: 'Clone and build',
        code: 'git clone https://github.com/swedishembedded/sven.git\ncd sven\nmake release',
      },
      {
        label: 'Install binary',
        code: 'sudo cp target/release/sven /usr/local/bin/',
      },
    ],
    note: (
      <p className="text-xs text-text-secondary">
        Requires Rust stable 1.75+. Build time ~2 min on modern hardware with sccache.
      </p>
    ),
  },
  macos: {
    steps: [
      {
        label: 'Download the universal binary',
        code: 'curl -L https://github.com/swedishembedded/sven/releases/latest/download/sven-darwin-universal -o sven',
      },
      {
        label: 'Make executable and install',
        code: 'chmod +x sven\nsudo mv sven /usr/local/bin/',
      },
      {
        label: 'Set your API key and run',
        code: 'export OPENAI_API_KEY="sk-..."\nsven',
      },
    ],
    note: (
      <p className="text-xs text-text-secondary">
        Universal binary supports both Apple Silicon (M1/M2/M3) and Intel Macs.
        macOS builds are experimental and may not be available on every release.
      </p>
    ),
  },
}

export default function InstallSection() {
  const [active, setActive] = useState<TabId>('oneliner')
  const ref = useRef<HTMLDivElement>(null)
  const inView = useInView(ref, { once: true, margin: '-80px' })

  const content = TAB_CONTENT[active]

  return (
    <section id="install" ref={ref} className="py-24 lg:py-32 border-t border-bg-border">
      <div className="max-w-4xl mx-auto px-4 sm:px-6 lg:px-8">
        {/* Header */}
        <motion.div
          initial={{ opacity: 0, y: 20 }}
          animate={inView ? { opacity: 1, y: 0 } : {}}
          transition={{ duration: 0.55 }}
          className="text-center mb-12"
        >
          <p className="text-xs font-mono uppercase tracking-widest text-accent-blue mb-4">Get started</p>
          <h2 className="section-heading mb-4">Up and running in 10 seconds.</h2>
          <p className="section-subheading max-w-xl mx-auto">
            A single binary. No Node.js. No Python. No Docker. Just download and run.
          </p>
        </motion.div>

        <motion.div
          initial={{ opacity: 0, y: 24 }}
          animate={inView ? { opacity: 1, y: 0 } : {}}
          transition={{ duration: 0.55, delay: 0.1 }}
          className="rounded-xl border border-bg-border overflow-hidden shadow-card"
          style={{ background: '#0d0d14' }}
        >
          {/* Tab bar */}
          <div className="flex overflow-x-auto border-b border-bg-border" style={{ background: '#0a0a0f' }}>
            {TABS.map((tab) => (
              <button
                key={tab.id}
                onClick={() => setActive(tab.id)}
                className={`flex items-center gap-2 px-5 py-3.5 text-sm font-medium whitespace-nowrap transition-all border-b-2 focus:outline-none ${
                  active === tab.id
                    ? 'text-accent-blue border-accent-blue bg-bg-elevated'
                    : 'text-text-secondary border-transparent hover:text-text-primary hover:bg-bg-elevated/50'
                }`}
              >
                <span className="w-4 h-4">{tab.icon}</span>
                {tab.label}
              </button>
            ))}
          </div>

          {/* Steps */}
          <div className="p-6 space-y-5">
            {content.steps.map((step, i) => (
              <div key={i}>
                {step.label && (
                  <p className="text-xs font-medium text-text-secondary mb-2">
                    <span className="inline-flex items-center justify-center w-4 h-4 rounded-full text-xs font-mono mr-2"
                          style={{ background: 'rgba(91,141,238,0.15)', color: '#5b8dee' }}>
                      {i + 1}
                    </span>
                    {step.label}
                  </p>
                )}
                <CodeBlock code={step.code} comment={step.comment} />
              </div>
            ))}
            {content.note && (
              <div className="pt-2 border-t border-bg-border">{content.note}</div>
            )}
          </div>
        </motion.div>

        {/* Platform badges + download */}
        <motion.div
          initial={{ opacity: 0, y: 16 }}
          animate={inView ? { opacity: 1, y: 0 } : {}}
          transition={{ duration: 0.5, delay: 0.25 }}
          className="flex flex-col sm:flex-row items-center justify-between gap-6 mt-8"
        >
          <div className="flex flex-wrap gap-2 justify-center sm:justify-start">
            {PLATFORM_BADGES.map((b) => (
              <span
                key={b.label}
                className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-mono border border-bg-border text-text-secondary bg-bg-elevated"
              >
                {b.icon}
                {b.label}
              </span>
            ))}
          </div>
          <a
            href="https://github.com/swedishembedded/sven/releases/latest"
            target="_blank"
            rel="noopener noreferrer"
            className="btn-primary flex-shrink-0"
          >
            <DownloadIcon />
            Download Latest Release
          </a>
        </motion.div>
      </div>
    </section>
  )
}

function CodeBlock({ code, comment }: { code: string; comment?: string }) {
  const [copied, setCopied] = useState(false)

  const handleCopy = () => {
    navigator.clipboard.writeText(code).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    }).catch(() => {})
  }

  return (
    <div className="relative group rounded-lg border border-bg-border overflow-hidden" style={{ background: '#0a0a0f' }}>
      <div className="flex items-center gap-2 px-4 py-2.5 border-b border-bg-border" style={{ background: '#0d0d14' }}>
        <span className="w-2 h-2 rounded-full" style={{ background: '#1e1e2e' }} />
        <span className="w-2 h-2 rounded-full" style={{ background: '#1e1e2e' }} />
        <span className="w-2 h-2 rounded-full" style={{ background: '#1e1e2e' }} />
      </div>
      <div className="relative px-4 py-4">
        <pre className="font-mono text-sm leading-6 overflow-x-auto pr-10">
          {code.split('\n').map((line, i) => (
            <div key={i}>
              <span style={{ color: '#5b8dee' }}>$ </span>
              <span style={{ color: '#e8e8e8' }}>{line}</span>
            </div>
          ))}
          {comment && (
            <div className="mt-1">
              <span style={{ color: '#555568' }}>{comment}</span>
            </div>
          )}
        </pre>
        <button
          onClick={handleCopy}
          className="absolute top-3 right-3 p-1.5 rounded text-text-dim hover:text-text-secondary transition-colors opacity-0 group-hover:opacity-100 focus:opacity-100 focus:outline-none"
          title="Copy"
        >
          {copied ? (
            <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="#4ade80" strokeWidth={2}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
            </svg>
          ) : (
            <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
              <path strokeLinecap="round" strokeLinejoin="round" d="M8 16H6a2 2 0 01-2-2V6a2 2 0 012-2h8a2 2 0 012 2v2m-6 12h8a2 2 0 002-2v-8a2 2 0 00-2-2h-8a2 2 0 00-2 2v8a2 2 0 002 2z" />
            </svg>
          )}
        </button>
      </div>
    </div>
  )
}

const PLATFORM_BADGES = [
  { label: 'Linux x86_64', icon: '🐧' },
  { label: 'Linux aarch64', icon: '🐧' },
  { label: 'macOS Universal', icon: '🍎' },
]

function BoltIcon() {
  return (
    <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M13 10V3L4 14h7v7l9-11h-7z" />
    </svg>
  )
}

function PackageIcon() {
  return (
    <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M20 7l-8-4-8 4m16 0l-8 4m8-4v10l-8 4m0-10L4 7m8 4v10M4 7v10l8 4" />
    </svg>
  )
}

function CodeIcon() {
  return (
    <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M10 20l4-16m4 4l4 4-4 4M6 16l-4-4 4-4" />
    </svg>
  )
}

function AppleIcon() {
  return (
    <svg className="w-4 h-4" fill="currentColor" viewBox="0 0 814 1000">
      <path d="M788.1 340.9c-5.8 4.5-108.2 62.2-108.2 190.5 0 148.4 130.3 200.9 134.2 202.2-.6 3.2-20.7 71.9-68.7 141.9-42.8 61.6-87.5 123.1-155.5 123.1s-85.5-39.5-164-39.5c-76 0-103.7 40.8-165.9 40.8s-105-57.8-155.5-127.4C46 790.7 0 663 0 541.8c0-207.5 135.4-317.3 269-317.3 71 0 130.5 46.4 174.9 46.4 42.7 0 109.2-49 192.9-49 31.3 0 108.2 2.6 168.2 75.5zm-174.9-127.4c-6.5-36.2 20.7-73.6 51.3-99.6 35.7-29.9 90.4-52.5 127.7-52.5 2.6 0 5.2 0 7.8.6-2.6 35.1-16.8 70.1-46.4 97.3-26.7 25.3-80.8 53.6-140.4 54.2z"/>
    </svg>
  )
}

function DownloadIcon() {
  return (
    <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4" />
    </svg>
  )
}
