import { motion } from 'framer-motion'
import { useInView } from 'framer-motion'
import { useRef } from 'react'

interface Feature {
  icon: React.ReactNode
  title: string
  description: string
  tag?: string
  tagColor?: string
}

const FEATURES: Feature[] = [
  {
    icon: <TerminalIcon />,
    title: 'Terminal-First TUI',
    description:
      'Full-screen keyboard-driven interface with vim-style navigation, live streaming output, and real-time tool visibility. No browser. No Electron. Just your terminal.',
  },
  {
    icon: <PipeIcon />,
    title: 'Headless & CI-Ready',
    description:
      'Reads from stdin, writes clean text to stdout, exits with meaningful codes. Pipe it, script it, schedule it. Runs identically in CI without changing a single line.',
  },
  {
    icon: <ChipIcon />,
    title: 'Native GDB Integration',
    description:
      'The first AI agent that connects to real hardware. Start debug servers, set breakpoints, inspect memory and registers — entirely autonomously.',
    tag: 'Industry First',
    tagColor: '#e6b428',
  },
  {
    icon: <NetworkIcon />,
    title: 'Agent-to-Agent P2P',
    description:
      'Agents discover each other via mDNS on your LAN or over the internet through a relay. Delegate subtasks and assemble results — no central server required.',
    tag: 'Unique',
    tagColor: '#5b8dee',
  },
  {
    icon: <WorkflowIcon />,
    title: 'Markdown Workflows',
    description:
      'Define multi-step pipelines in plain Markdown with YAML frontmatter, template variables, and per-step configuration. Version-control your automation.',
  },
  {
    icon: <ProvidersIcon />,
    title: '35+ Model Providers',
    description:
      'OpenAI, Anthropic, Google, Azure, AWS Bedrock, Groq, Ollama, and more. Switch providers with a single config line. No lock-in.',
  },
]

export default function FeatureGrid() {
  const ref = useRef<HTMLDivElement>(null)
  const inView = useInView(ref, { once: true, margin: '-60px' })

  return (
    <section id="features" ref={ref} className="py-24 lg:py-32">
      <div className="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
        {/* Section header */}
        <motion.div
          initial={{ opacity: 0, y: 20 }}
          animate={inView ? { opacity: 1, y: 0 } : {}}
          transition={{ duration: 0.55 }}
          className="text-center mb-16"
        >
          <p className="text-xs font-mono uppercase tracking-widest text-accent-blue mb-4">Everything you need</p>
          <h2 className="section-heading mb-4">Built for engineers who live in the terminal.</h2>
          <p className="section-subheading max-w-2xl mx-auto">
            Sven combines a polished interactive experience with rock-solid headless automation — in
            one binary that installs in seconds and requires no configuration to get started.
          </p>
        </motion.div>

        {/* Grid */}
        <div className="grid sm:grid-cols-2 lg:grid-cols-3 gap-5">
          {FEATURES.map((feature, i) => (
            <motion.div
              key={feature.title}
              initial={{ opacity: 0, y: 24 }}
              animate={inView ? { opacity: 1, y: 0 } : {}}
              transition={{ duration: 0.5, delay: 0.05 + i * 0.07 }}
              className="card group hover:border-accent-blue/30 hover:bg-bg-hover transition-all duration-200"
            >
              <div className="flex items-start gap-4 mb-4">
                <div
                  className="flex-shrink-0 w-10 h-10 rounded-lg flex items-center justify-center"
                  style={{ background: 'rgba(91,141,238,0.12)' }}
                >
                  <span style={{ color: '#5b8dee' }}>{feature.icon}</span>
                </div>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 flex-wrap">
                    <h3 className="text-base font-semibold text-text-primary">{feature.title}</h3>
                    {feature.tag && (
                      <span
                        className="text-xs font-mono px-2 py-0.5 rounded-full"
                        style={{
                          color: feature.tagColor,
                          background: `${feature.tagColor}18`,
                          border: `1px solid ${feature.tagColor}30`,
                        }}
                      >
                        {feature.tag}
                      </span>
                    )}
                  </div>
                </div>
              </div>
              <p className="text-sm text-text-secondary leading-relaxed">{feature.description}</p>
            </motion.div>
          ))}
        </div>
      </div>
    </section>
  )
}

function TerminalIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z" />
    </svg>
  )
}

function PipeIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M4 6h16M4 10h16M4 14h16M4 18h16" />
    </svg>
  )
}

function ChipIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M9 3H7a2 2 0 00-2 2v2M9 3h6M9 3v0M15 3h2a2 2 0 012 2v2M3 9v2m0 0v2M3 11h0M21 9v2m0 0v2M21 11h0M9 21H7a2 2 0 01-2-2v-2m4 4h6m-6 0v0M15 21h2a2 2 0 002-2v-2M7 9h10v6H7z" />
    </svg>
  )
}

function NetworkIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M21 12a9 9 0 01-9 9m9-9a9 9 0 00-9-9m9 9H3m9 9a9 9 0 01-9-9m9 9c1.657 0 3-4.03 3-9s-1.343-9-3-9m0 18c-1.657 0-3-4.03-3-9s1.343-9 3-9m-9 9a9 9 0 019-9" />
    </svg>
  )
}

function WorkflowIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2m-6 9l2 2 4-4" />
    </svg>
  )
}

function ProvidersIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.8}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M19 11H5m14 0a2 2 0 012 2v6a2 2 0 01-2 2H5a2 2 0 01-2-2v-6a2 2 0 012-2m14 0V9a2 2 0 00-2-2M5 11V9a2 2 0 012-2m0 0V5a2 2 0 012-2h6a2 2 0 012 2v2M7 7h10" />
    </svg>
  )
}
