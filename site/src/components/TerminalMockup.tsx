const CHIP_LOGO = [
  '    ╷    ╷    ╷    ╷   ',
  ' ╔══╧════╧════╧════╧══╗',
  ' ║   ╔════╗  ╔════╗   ║',
  '─╢   ║    ║  ║    ║   ╟─',
  '─╢   ╚════╝  ╚════╝   ╟─',
  '─╢                    ╟─',
  '─╢   ╔════╗  ╔════╗   ╟─',
  '─╢   ║    ║  ║    ║   ╟─',
  ' ║   ╚════╝  ╚════╝   ║',
  ' ╚══╤════╤════╤════╤══╝',
  '    ╵    ╵    ╵    ╵   ',
]

const CONVERSATION = [
  { role: 'user', text: '❯ sven' },
  { role: 'info', text: ' ⬡ sven ' },
  { role: 'space', text: '' },
  { role: 'user', text: '❯ Analyze the auth module and suggest improvements' },
  { role: 'space', text: '' },
  { role: 'tool', text: '⚙  read_file  src/auth/mod.rs' },
  { role: 'tool', text: '⚙  grep  "unsafe" src/auth/' },
  { role: 'tool', text: '⚙  run_terminal_command  cargo clippy -p auth' },
  { role: 'space', text: '' },
  { role: 'agent', text: '●  Found 3 issues in the auth module:' },
  { role: 'agent', text: '   1. Missing rate limiting on login attempts' },
  { role: 'agent', text: '   2. Token expiry not enforced in refresh path' },
  { role: 'agent', text: '   3. Plaintext fallback in legacy compat layer' },
  { role: 'space', text: '' },
  { role: 'agent', text: '   Shall I fix all three? [y/n] ▌' },
]

function ChipLine({ line }: { line: string }) {
  const isPinLine = line.startsWith('─╢') || line.startsWith('─')
  const isYellow = !isPinLine || line.includes('╔') || line.includes('╚') || line.includes('╟')

  if (isPinLine) {
    return (
      <span>
        <span style={{ color: '#e6b428' }}>─╢</span>
        <span style={{ color: '#3c78dc' }}>{line.slice(2, -2)}</span>
        <span style={{ color: '#e6b428' }}>╟─</span>
      </span>
    )
  }

  const hasBlue = line.includes('╔════╗') || line.includes('╚════╝')
  if (hasBlue && !line.startsWith('    ')) {
    const parts = line.split(/(╔════╗|╚════╝)/g)
    return (
      <span>
        {parts.map((part, i) =>
          part === '╔════╗' || part === '╚════╝' ? (
            <span key={i} style={{ color: '#3c78dc' }}>{part}</span>
          ) : (
            <span key={i} style={{ color: isYellow ? '#e6b428' : '#aaaaaa' }}>{part}</span>
          )
        )}
      </span>
    )
  }

  return <span style={{ color: '#888' }}>{line}</span>
}

function ConvLine({ role, text }: { role: string; text: string }) {
  if (role === 'space') return <div className="h-1" />
  if (role === 'user') return <div style={{ color: '#4ade80' }} className="font-mono text-xs leading-5">{text}</div>
  if (role === 'info') return <div style={{ color: '#5b8dee' }} className="font-mono text-xs leading-5">{text}</div>
  if (role === 'tool') return <div style={{ color: '#c8a03c' }} className="font-mono text-xs leading-5">{text}</div>
  if (role === 'agent') return (
    <div className="font-mono text-xs leading-5" style={{ color: '#c8c8d8' }}>
      {text.endsWith('▌') ? (
        <>
          {text.slice(0, -1)}
          <span className="terminal-cursor" style={{ width: '6px', height: '13px' }} />
        </>
      ) : text}
    </div>
  )
  return null
}

export default function TerminalMockup() {
  return (
    <div
      className="relative w-full rounded-xl overflow-hidden shadow-[0_0_60px_rgba(91,141,238,0.2)] border border-bg-border"
      style={{ background: '#0d0d0d' }}
    >
      {/* Title bar */}
      <div
        className="flex items-center gap-2 px-4 py-3 border-b border-bg-border"
        style={{ background: '#111116' }}
      >
        <span className="w-3 h-3 rounded-full" style={{ background: '#f87171' }} />
        <span className="w-3 h-3 rounded-full" style={{ background: '#f59e0b' }} />
        <span className="w-3 h-3 rounded-full" style={{ background: '#4ade80' }} />
        <span className="ml-3 text-xs font-mono" style={{ color: '#555568' }}>
          sven — research mode
        </span>
        <span className="ml-auto text-xs font-mono px-2 py-0.5 rounded" style={{ color: '#5b8dee', background: 'rgba(91,141,238,0.1)' }}>
          gpt-4o
        </span>
      </div>

      {/* Content */}
      <div className="flex flex-col md:flex-row" style={{ minHeight: '360px' }}>
        {/* Left: chip logo pane */}
        <div
          className="flex flex-col items-center justify-center px-6 py-6 border-b md:border-b-0 md:border-r border-bg-border flex-shrink-0"
          style={{ minWidth: '240px', background: '#0a0a0f' }}
        >
          <div className="text-xs leading-5 select-none whitespace-pre" style={{ fontFamily: '"JetBrains Mono", "SF Mono", "Fira Code", Consolas, monospace' }}>
            {CHIP_LOGO.map((line, i) => (
              <div key={i}>
                <ChipLine line={line} />
              </div>
            ))}
          </div>
          <div className="mt-3 font-mono text-xs" style={{ color: '#555568' }}>
            sven
          </div>
          <div className="mt-4 flex flex-col gap-1 w-full">
            <div className="flex items-center gap-2 text-xs font-mono" style={{ color: '#555568' }}>
              <span className="w-2 h-2 rounded-full" style={{ background: '#4ade80' }} />
              <span>agent mode</span>
            </div>
            <div className="flex items-center gap-2 text-xs font-mono" style={{ color: '#555568' }}>
              <span className="w-2 h-2 rounded-full" style={{ background: '#5b8dee' }} />
              <span>35 providers</span>
            </div>
          </div>
        </div>

        {/* Right: conversation */}
        <div className="flex-1 p-4 overflow-hidden">
          {CONVERSATION.map((line, i) => (
            <ConvLine key={i} role={line.role} text={line.text} />
          ))}
        </div>
      </div>

      {/* Status bar */}
      <div
        className="flex items-center gap-3 px-4 py-2 border-t border-bg-border font-mono text-xs"
        style={{ background: '#0d0d14', color: '#555568' }}
      >
        <span style={{ color: '#5b8dee' }}>⬡ sven</span>
        <span>·</span>
        <span>research</span>
        <span className="ml-auto flex items-center gap-1">
          <span className="w-1.5 h-1.5 rounded-full animate-pulse-dot" style={{ background: '#4ade80' }} />
          ready
        </span>
      </div>
    </div>
  )
}
