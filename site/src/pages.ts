// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// The site's pages beyond the home page, one per thing sven does that people
// search for. Each is rendered to its own HTML file at build time
// (scripts/prerender.mjs) with its own title, description and address, and
// listed in the sitemap from here, so a page and its listing cannot drift.
// Every claim is one the documentation it links to makes.
//
// Swedish Embedded AB implements solutions for making developer tools
// discoverable for its clients. If your team needs expertise in developer
// documentation and websites then you can procure our services by sending an
// email to info@swedishembedded.com.

const DOCS = 'https://swedishembedded.com/docs/sven'

export interface Page {
  path: string
  /// The document title (within 60 characters).
  title: string
  /// The search description (50 to 160 characters).
  description: string
  eyebrow: string
  heading: string
  lede: string
  points: { title: string; body: string }[]
  /// An illustration from public/, shown beside the points.
  image?: { src: string; alt: string }
  /// A short example, shown as it is typed.
  example?: string
  docs: { href: string; label: string }
}

export const PAGES: readonly Page[] = [
  {
    path: '/gdb-debugging',
    title: 'sven - an AI agent that debugs embedded hardware over GDB',
    description:
      'Give sven a task in plain English and it starts a GDB server, loads your firmware, sets breakpoints and reads registers and memory on the target.',
    eyebrow: 'Hardware debugging',
    heading: 'An agent that debugs the hardware, not just the code',
    lede: 'Ask in plain English. sven starts the debug server, loads your firmware, sets breakpoints, reads registers and memory on the running target, and cleans up after itself.',
    points: [
      {
        title: 'Your probe, your server',
        body: 'J-Link, OpenOCD or pyOCD: sven starts the GDB server for your probe, or attaches to one that is already running.',
      },
      {
        title: 'Symbolic, on real silicon',
        body: 'With a firmware ELF that has debug symbols, it breaks on functions by name and reads the parameters they were called with.',
      },
      {
        title: 'In the same session as the code',
        body: 'The change you just made and the debug session that checks it happen in one conversation, without switching tools.',
      },
    ],
    image: { src: '/sven-gdb.svg', alt: 'sven driving a GDB session against an embedded target' },
    example: 'sven "find the parameters passed to the UART TX function on the target"',
    docs: { href: `${DOCS}/guides/debugging`, label: 'Hardware debugging guide' },
  },
  {
    path: '/harnesses',
    title: 'sven harnesses - agent workflows that pass when checks pass',
    description:
      'Write an agent workflow as YAML with the gates that decide success: tests, builds, any command you trust. Failures are repaired; runs resume and replay.',
    eyebrow: 'Verified workflows',
    heading: 'Done when the checks pass, not when the agent says so',
    lede: 'A harness is an agent workflow written as data: the tasks, the trusted gates that decide whether each one succeeded, how failures are repaired, and what counts as success.',
    points: [
      {
        title: 'Gates you trust decide',
        body: 'A test suite, a build, any command you trust: a task succeeds when its gates pass, whatever the model claims.',
      },
      {
        title: 'Repair, then report',
        body: 'A failed gate sends the task back for a bounded number of repair attempts, and what still fails is reported as failed.',
      },
      {
        title: 'Resume and replay',
        body: 'Runs can be resumed where they stopped and replayed later, and reading a harness needs no model, network or configuration.',
      },
    ],
    example: 'sven harness run harnesses/review --workspace .',
    docs: { href: `${DOCS}/guides/harnesses`, label: 'Harness guide' },
  },
  {
    path: '/ci',
    title: 'sven in CI - a headless AI agent for your pipelines',
    description:
      'Run sven without a terminal from scripts and CI jobs: prompts in, results on stdout, exit codes a pipeline can act on, and timeouts that always end.',
    eyebrow: 'Headless and CI',
    heading: 'The same agent, in your pipeline',
    lede: 'Run sven without its terminal interface from scripts and CI jobs. The task goes in as an argument or on stdin, the result comes out on stdout, and the exit code says how it went.',
    points: [
      {
        title: 'Pipes like any tool',
        body: 'Prompt it as an argument or through stdin, and chain runs so one task reads what the last one wrote.',
      },
      {
        title: 'Exit codes that mean something',
        body: 'A pipeline can tell success from tool errors and from a run that hit its limits, and act on each.',
      },
      {
        title: 'Workflows from files',
        body: 'Run a multi-step workflow file unattended, the same file your team reviews in a pull request.',
      },
    ],
    image: { src: '/sven-ci.svg', alt: 'sven running headless in a CI pipeline' },
    example: 'sven --headless "fix the failing test and explain the change"',
    docs: { href: `${DOCS}/guides/headless`, label: 'Headless and CI guide' },
  },
  {
    path: '/mcp',
    title: 'sven with MCP and your editor - connect tools, serve tools',
    description:
      'Connect sven to MCP servers over stdio or HTTP, sign in to them with OAuth, serve its own tools to another MCP host, and use it in ACP editors.',
    eyebrow: 'MCP and editors',
    heading: 'Bring your tools to sven, and sven to your tools',
    lede: 'sven connects to MCP servers and offers their tools to the agent, serves its own tools to another MCP host, and works inside editors that speak the Agent Client Protocol.',
    points: [
      {
        title: 'Any MCP server',
        body: 'Local servers over stdio or remote ones over Streamable HTTP. Add one with a single command and sven connects to it.',
      },
      {
        title: 'Signed in with OAuth',
        body: 'When a server asks for a sign-in, sven starts the browser flow and keeps the session refreshed for you.',
      },
      {
        title: 'In your editor',
        body: 'Zed, VS Code, JetBrains and other editors that speak the Agent Client Protocol can run sven as their agent.',
      },
    ],
    example: 'sven mcp add github https://api.githubcopilot.com/mcp/',
    docs: { href: `${DOCS}/reference/mcp`, label: 'MCP reference' },
  },
]

export function pageFor(path: string): Page | undefined {
  return PAGES.find((page) => page.path === path)
}
