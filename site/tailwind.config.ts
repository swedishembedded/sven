import type { Config } from 'tailwindcss'

const config: Config = {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        bg: {
          base: '#0a0a0f',
          elevated: '#12121a',
          card: '#0f0f17',
          border: '#1e1e2e',
          hover: '#1a1a28',
        },
        accent: {
          blue: '#5b8dee',
          'blue-dim': '#3d6bc4',
          'blue-hover': '#4a7de0',
          yellow: '#e6b428',
          'yellow-dim': '#b88e1e',
        },
        text: {
          primary: '#e8e8e8',
          secondary: '#888898',
          dim: '#555568',
          code: '#5b8dee',
        },
        status: {
          green: '#4ade80',
          amber: '#f59e0b',
          red: '#f87171',
        },
      },
      fontFamily: {
        sans: ['Inter', '-apple-system', 'BlinkMacSystemFont', '"Segoe UI"', 'system-ui', 'sans-serif'],
        mono: ['"JetBrains Mono"', '"SF Mono"', '"Fira Code"', '"Fira Mono"', '"Roboto Mono"', 'Consolas', 'monospace'],
      },
      animation: {
        'pulse-dot': 'pulseDot 2s ease-in-out infinite',
        'fade-in': 'fadeIn 0.6s ease-out',
        'slide-up': 'slideUp 0.6s ease-out',
        blink: 'blink 1.2s step-end infinite',
      },
      keyframes: {
        pulseDot: {
          '0%, 100%': { opacity: '1' },
          '50%': { opacity: '0.3' },
        },
        fadeIn: {
          from: { opacity: '0' },
          to: { opacity: '1' },
        },
        slideUp: {
          from: { opacity: '0', transform: 'translateY(24px)' },
          to: { opacity: '1', transform: 'translateY(0)' },
        },
        blink: {
          '0%, 100%': { opacity: '1' },
          '50%': { opacity: '0' },
        },
      },
      backgroundImage: {
        'gradient-radial': 'radial-gradient(var(--tw-gradient-stops))',
        'hero-glow': 'radial-gradient(ellipse 80% 50% at 50% -20%, rgba(91,141,238,0.15), transparent)',
        'grid-pattern':
          "url(\"data:image/svg+xml,%3Csvg width='60' height='60' viewBox='0 0 60 60' xmlns='http://www.w3.org/2000/svg'%3E%3Cg fill='none' fill-rule='evenodd'%3E%3Cg fill='%231e1e2e' fill-opacity='1'%3E%3Cpath d='M36 34v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zm0-30V0h-2v4h-4v2h4v4h2V6h4V4h-4zM6 34v-4H4v4H0v2h4v4h2v-4h4v-2H6zM6 4V0H4v4H0v2h4v4h2V6h4V4H6z'/%3E%3C/g%3E%3C/g%3E%3C/svg%3E\")",
      },
      boxShadow: {
        glow: '0 0 40px rgba(91,141,238,0.15)',
        'glow-sm': '0 0 20px rgba(91,141,238,0.1)',
        card: '0 1px 3px rgba(0,0,0,0.5), 0 4px 24px rgba(0,0,0,0.3)',
      },
    },
  },
  plugins: [],
}

export default config
