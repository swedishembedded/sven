import Logo from './Logo'

const PRODUCT_LINKS = [
  { label: 'Features', href: '#features' },
  { label: 'Install', href: '#install' },
  { label: 'Documentation', href: 'https://github.com/swedishembedded/sven/tree/main/docs', external: true },
  { label: 'Configuration', href: 'https://github.com/swedishembedded/sven/blob/main/docs/05-configuration.md', external: true },
  { label: 'CI Pipelines', href: 'https://github.com/swedishembedded/sven/blob/main/docs/04-ci-pipeline.md', external: true },
]

const COMMUNITY_LINKS = [
  { label: 'GitHub', href: 'https://github.com/swedishembedded/sven', external: true },
  { label: 'Releases', href: 'https://github.com/swedishembedded/sven/releases', external: true },
  { label: 'Issues', href: 'https://github.com/swedishembedded/sven/issues', external: true },
  { label: 'Discussions', href: 'https://github.com/swedishembedded/sven/discussions', external: true },
  { label: 'Changelog', href: 'https://github.com/swedishembedded/sven/releases', external: true },
]

const COMPANY_LINKS = [
  { label: 'Swedish Embedded AB', href: 'https://swedishembedded.com', external: true },
  { label: 'Apache 2.0 License', href: 'https://github.com/swedishembedded/sven/blob/main/LICENSE', external: true },
]

export default function Footer() {
  return (
    <footer className="border-t border-bg-border bg-bg-elevated/30">
      <div className="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
        {/* Main footer content */}
        <div className="py-16 grid grid-cols-2 md:grid-cols-4 gap-10">
          {/* Brand column */}
          <div className="col-span-2 md:col-span-1">
            <Logo size="md" className="mb-4" />
            <p className="text-sm text-text-secondary leading-relaxed mb-5 max-w-xs">
              A keyboard-driven AI coding agent for the terminal. Built with Rust by{' '}
              <a
                href="https://swedishembedded.com"
                target="_blank"
                rel="noopener noreferrer"
                className="text-text-primary hover:text-accent-blue transition-colors"
              >
                Swedish Embedded AB
              </a>
              .
            </p>
            <div className="flex items-center gap-3">
              <a
                href="https://github.com/swedishembedded/sven"
                target="_blank"
                rel="noopener noreferrer"
                className="w-8 h-8 rounded-lg flex items-center justify-center border border-bg-border text-text-secondary hover:text-text-primary hover:border-accent-blue/40 transition-all"
                aria-label="GitHub"
              >
                <GitHubIcon />
              </a>
            </div>
          </div>

          {/* Product */}
          <div>
            <h3 className="text-xs font-semibold uppercase tracking-widest text-text-dim mb-4">Product</h3>
            <ul className="space-y-2.5">
              {PRODUCT_LINKS.map((link) => (
                <li key={link.label}>
                  <a
                    href={link.href}
                    {...(link.external ? { target: '_blank', rel: 'noopener noreferrer' } : {})}
                    className="text-sm text-text-secondary hover:text-text-primary transition-colors"
                  >
                    {link.label}
                  </a>
                </li>
              ))}
            </ul>
          </div>

          {/* Community */}
          <div>
            <h3 className="text-xs font-semibold uppercase tracking-widest text-text-dim mb-4">Community</h3>
            <ul className="space-y-2.5">
              {COMMUNITY_LINKS.map((link) => (
                <li key={link.label}>
                  <a
                    href={link.href}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-sm text-text-secondary hover:text-text-primary transition-colors"
                  >
                    {link.label}
                  </a>
                </li>
              ))}
            </ul>
          </div>

          {/* Company */}
          <div>
            <h3 className="text-xs font-semibold uppercase tracking-widest text-text-dim mb-4">Company</h3>
            <ul className="space-y-2.5">
              {COMPANY_LINKS.map((link) => (
                <li key={link.label}>
                  <a
                    href={link.href}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-sm text-text-secondary hover:text-text-primary transition-colors"
                  >
                    {link.label}
                  </a>
                </li>
              ))}
            </ul>
          </div>
        </div>

        {/* Bottom bar */}
        <div className="py-6 border-t border-bg-border flex flex-col sm:flex-row items-center justify-between gap-3">
          <p className="text-xs text-text-dim font-mono">
            Built with Rust.{' '}
            <a
              href="https://github.com/swedishembedded/sven/blob/main/LICENSE"
              target="_blank"
              rel="noopener noreferrer"
              className="hover:text-text-secondary transition-colors"
            >
              Apache 2.0 License.
            </a>
          </p>
          <p className="text-xs text-text-dim font-mono">
            © {new Date().getFullYear()} Swedish Embedded AB
          </p>
        </div>
      </div>
    </footer>
  )
}

function GitHubIcon() {
  return (
    <svg className="w-4 h-4" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true">
      <path
        fillRule="evenodd"
        d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
        clipRule="evenodd"
      />
    </svg>
  )
}
