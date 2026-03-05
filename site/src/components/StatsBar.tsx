import { motion } from 'framer-motion'
import { useInView } from 'framer-motion'
import { useRef } from 'react'

const STATS = [
  { value: '35+', label: 'Model Providers', sub: 'OpenAI, Anthropic, Ollama & more' },
  { value: '1', label: 'Single Binary', sub: 'TUI, headless, and node in one' },
  { value: '0', label: 'Runtime Dependencies', sub: 'Pure Rust, ships ready to run' },
  { value: '∞', label: 'Workflow Scale', sub: 'From one-liners to full CI pipelines' },
]

export default function StatsBar() {
  const ref = useRef<HTMLDivElement>(null)
  const inView = useInView(ref, { once: true, margin: '-80px' })

  return (
    <section ref={ref} className="border-y border-bg-border bg-bg-elevated/50">
      <div className="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8 py-12">
        <div className="grid grid-cols-2 lg:grid-cols-4 gap-8">
          {STATS.map((stat, i) => (
            <motion.div
              key={stat.label}
              initial={{ opacity: 0, y: 16 }}
              animate={inView ? { opacity: 1, y: 0 } : {}}
              transition={{ duration: 0.5, delay: i * 0.08 }}
              className="text-center"
            >
              <div
                className="text-4xl font-extrabold mb-1 font-mono"
                style={{
                  background: 'linear-gradient(135deg, #e8e8e8 0%, #9898b8 100%)',
                  WebkitBackgroundClip: 'text',
                  WebkitTextFillColor: 'transparent',
                  backgroundClip: 'text',
                }}
              >
                {stat.value}
              </div>
              <div className="text-sm font-semibold text-text-primary mb-0.5">{stat.label}</div>
              <div className="text-xs text-text-dim">{stat.sub}</div>
            </motion.div>
          ))}
        </div>

        <motion.div
          initial={{ opacity: 0 }}
          animate={inView ? { opacity: 1 } : {}}
          transition={{ duration: 0.5, delay: 0.4 }}
          className="flex flex-wrap items-center justify-center gap-3 mt-10 pt-8 border-t border-bg-border"
        >
          {BADGES.map((b) => (
            <span
              key={b.label}
              className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-mono border border-bg-border text-text-secondary bg-bg-card"
            >
              <span style={{ color: b.color }}>{b.icon}</span>
              {b.label}
            </span>
          ))}
        </motion.div>
      </div>
    </section>
  )
}

const BADGES = [
  { label: 'Linux x86_64', icon: '🐧', color: '#e6b428' },
  { label: 'Linux aarch64', icon: '🐧', color: '#e6b428' },
  { label: 'macOS Universal', icon: '🍎', color: '#888' },
  { label: 'Written in Rust', icon: '⚙', color: '#f59e0b' },
  { label: 'MCP Compatible', icon: '◈', color: '#4ade80' },
]
