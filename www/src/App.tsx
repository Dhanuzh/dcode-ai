import { useState } from 'react'
import { CopyCommand } from './components/CopyCommand'
import { Terminal } from './components/Terminal'
import { DESKTOP_DOWNLOADS, DESKTOP_FEATURES, DESKTOP_REPO, FEATURES, INSTALL, PROVIDERS, REPO } from './data'

const NAV = [
  { href: '#desktop', label: 'Desktop' },
  { href: '#features', label: 'Features' },
  { href: '#providers', label: 'Providers' },
  { href: '#install', label: 'Install' },
]

function Nav() {
  return (
    <header className="sticky top-0 z-50 border-b border-line/70 bg-ink/80 backdrop-blur-md">
      <nav className="mx-auto flex max-w-6xl items-center gap-6 px-6 py-4">
        <a href="#top" className="flex items-center gap-2.5 font-mono font-semibold tracking-tight">
          <span aria-hidden className="text-accent">
            ▚
          </span>
          dcode-ai
        </a>
        <div className="ml-auto hidden items-center gap-6 text-sm text-muted sm:flex">
          {NAV.map((item) => (
            <a key={item.href} href={item.href} className="transition hover:text-fg">
              {item.label}
            </a>
          ))}
        </div>
        <a
          href={REPO}
          target="_blank"
          rel="noreferrer"
          className="rounded-lg border border-line px-3.5 py-1.5 text-sm text-fg transition hover:border-accent hover:text-accent"
        >
          GitHub
        </a>
      </nav>
    </header>
  )
}

function Hero() {
  return (
    <section id="top" className="relative overflow-hidden">
      <div aria-hidden className="absolute inset-0 grid-bg" />
      <div aria-hidden className="absolute inset-x-0 top-0 h-[32rem] glow-accent" />

      <div className="relative mx-auto max-w-6xl px-6 pt-20 pb-16 sm:pt-28 sm:pb-24">
        <div className="mx-auto max-w-3xl text-center">
          <span className="inline-flex items-center gap-2 rounded-full border border-line bg-surface/80 px-3.5 py-1.5 font-mono text-xs text-muted">
            <span className="h-1.5 w-1.5 rounded-full bg-green" />
            Written in Rust · single static binary
          </span>

          <h1 className="mt-7 text-balance text-4xl font-semibold leading-[1.1] tracking-tight sm:text-6xl">
            <span className="text-gradient">The coding agent</span>
            <br />
            that lives in your terminal
          </h1>

          <p className="mx-auto mt-6 max-w-xl text-pretty text-lg leading-relaxed text-muted">
            No Electron. No browser shell. No JavaScript wrapper. Just a fast, local-first
            agent with streaming reasoning, visible tool calls and resumable sessions —
            right where your code already is.
          </p>

          <div className="mx-auto mt-9 max-w-xl">
            <CopyCommand command={INSTALL.unix} />
          </div>

          <div className="mt-5 flex flex-wrap items-center justify-center gap-3">
            <a
              href="#install"
              className="rounded-lg bg-accent px-5 py-2.5 text-sm font-semibold text-ink transition hover:bg-accent-dim"
            >
              Get started
            </a>
            <a
              href={REPO}
              target="_blank"
              rel="noreferrer"
              className="rounded-lg border border-line px-5 py-2.5 text-sm font-medium text-fg transition hover:border-accent hover:text-accent"
            >
              View source
            </a>
          </div>
        </div>

        <div className="mx-auto mt-16 max-w-3xl">
          <Terminal />
        </div>
      </div>
    </section>
  )
}

function Features() {
  return (
    <section id="features" className="mx-auto max-w-6xl scroll-mt-20 px-6 py-20 sm:py-28">
      <div className="max-w-2xl">
        <p className="font-mono text-sm text-accent">Features</p>
        <h2 className="mt-3 text-3xl font-semibold tracking-tight sm:text-4xl">
          Everything you expect from an agent — in a terminal
        </h2>
        <p className="mt-4 text-pretty leading-relaxed text-muted">
          Built for developers who want their tools fast, local and scriptable.
        </p>
      </div>

      <div className="mt-12 grid gap-px overflow-hidden rounded-xl border border-line bg-line sm:grid-cols-2 lg:grid-cols-3">
        {FEATURES.map((f) => (
          <div key={f.title} className="group bg-surface p-6 transition hover:bg-raised">
            <span
              aria-hidden
              className="inline-flex h-9 w-9 items-center justify-center rounded-lg border border-line bg-raised text-accent transition group-hover:border-accent/50"
            >
              {f.glyph}
            </span>
            <h3 className="mt-4 font-semibold tracking-tight">{f.title}</h3>
            <p className="mt-2 text-sm leading-relaxed text-muted">{f.body}</p>
          </div>
        ))}
      </div>
    </section>
  )
}

function Providers() {
  return (
    <section id="providers" className="scroll-mt-20 border-y border-line bg-surface/40">
      <div className="mx-auto max-w-6xl px-6 py-20 sm:py-24">
        <div className="grid items-center gap-12 lg:grid-cols-2">
          <div>
            <p className="font-mono text-sm text-accent">Providers</p>
            <h2 className="mt-3 text-3xl font-semibold tracking-tight sm:text-4xl">
              Bring your own model
            </h2>
            <p className="mt-4 text-pretty leading-relaxed text-muted">
              Switch provider or model inline, mid-session, without losing context.
              Log in with OAuth or drop in an API key — dcode-ai stores credentials
              locally and never proxies your traffic through a third party.
            </p>
            <div className="mt-7 max-w-md">
              <CopyCommand command="/model claude-sonnet-4-6" prompt="›" />
            </div>
          </div>

          <ul className="grid grid-cols-2 gap-px overflow-hidden rounded-xl border border-line bg-line">
            {PROVIDERS.map((p) => (
              <li
                key={p}
                className="flex items-center gap-2.5 bg-surface px-5 py-4 text-sm text-fg"
              >
                <span aria-hidden className="text-accent">
                  ◆
                </span>
                {p}
              </li>
            ))}
          </ul>
        </div>
      </div>
    </section>
  )
}

const TABS = [
  { id: 'unix', label: 'Linux / macOS', command: INSTALL.unix, prompt: '$' },
  { id: 'windows', label: 'Windows', command: INSTALL.windows, prompt: '>' },
  { id: 'local', label: 'No sudo', command: INSTALL.local, prompt: '$' },
  { id: 'source', label: 'From source', command: INSTALL.source, prompt: '$' },
] as const

function Install() {
  const [active, setActive] = useState<(typeof TABS)[number]['id']>('unix')
  const tab = TABS.find((t) => t.id === active) ?? TABS[0]

  return (
    <section id="install" className="mx-auto max-w-3xl scroll-mt-20 px-6 py-20 sm:py-28">
      <div className="text-center">
        <p className="font-mono text-sm text-accent">Install</p>
        <h2 className="mt-3 text-3xl font-semibold tracking-tight sm:text-4xl">
          One command. No runtime.
        </h2>
        <p className="mt-4 text-pretty leading-relaxed text-muted">
          Ships as a single static binary. Nothing to configure before your first prompt.
        </p>
      </div>

      <div className="mt-10 rounded-xl border border-line bg-surface p-2">
        <div role="tablist" aria-label="Install method" className="flex flex-wrap gap-1">
          {TABS.map((t) => (
            <button
              key={t.id}
              role="tab"
              aria-selected={t.id === active}
              onClick={() => setActive(t.id)}
              className={`rounded-lg px-3.5 py-2 text-sm transition focus:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
                t.id === active
                  ? 'bg-raised font-medium text-fg'
                  : 'text-muted hover:text-fg'
              }`}
            >
              {t.label}
            </button>
          ))}
        </div>
        <div className="p-2 pt-3">
          <CopyCommand command={tab.command} prompt={tab.prompt} />
        </div>
      </div>

      <p className="mt-5 text-center text-sm text-muted">
        Then run <code className="font-mono text-accent">dcode-ai</code> in any project.
        Building from source needs Rust 1.88+.
      </p>
    </section>
  )
}

function Desktop() {
  return (
    <section id="desktop" className="scroll-mt-20 border-y border-line bg-surface/40">
      <div className="mx-auto max-w-6xl px-6 py-20 sm:py-24">
        <div className="max-w-2xl">
          <p className="font-mono text-sm text-accent">Desktop</p>
          <h2 className="mt-3 text-3xl font-semibold tracking-tight sm:text-4xl">
            Prefer a window? There's DCode Desktop.
          </h2>
          <p className="mt-4 text-pretty leading-relaxed text-muted">
            DCode Desktop wraps the same Rust agent engine that powers dcode-ai in a
            chat-style app for Windows and Linux. Where dcode-ai stays terminal-first and
            single-binary, Desktop trades that for a windowed interface built around
            longer-running, multi-conversation work — persistent history, named
            assistants, unattended scheduled runs, and a UI for wiring up MCP servers
            instead of hand-editing config files. Same models, same tool-calling
            underneath; pick whichever surface fits the task.
          </p>
          <div className="mt-7 flex flex-wrap items-center gap-3">
            <a
              href={DESKTOP_DOWNLOADS.releases}
              target="_blank"
              rel="noreferrer"
              className="rounded-lg bg-accent px-5 py-2.5 text-sm font-semibold text-ink transition hover:bg-accent-dim"
            >
              Download for Windows / Linux
            </a>
            <a
              href={DESKTOP_REPO}
              target="_blank"
              rel="noreferrer"
              className="rounded-lg border border-line px-5 py-2.5 text-sm font-medium text-fg transition hover:border-accent hover:text-accent"
            >
              View source
            </a>
          </div>
        </div>

        <div className="mt-12 grid gap-px overflow-hidden rounded-xl border border-line bg-line sm:grid-cols-2 lg:grid-cols-3">
          {DESKTOP_FEATURES.map((f) => (
            <div key={f.title} className="group bg-surface p-6 transition hover:bg-raised">
              <span
                aria-hidden
                className="inline-flex h-9 w-9 items-center justify-center rounded-lg border border-line bg-raised text-accent transition group-hover:border-accent/50"
              >
                ◆
              </span>
              <h3 className="mt-4 font-semibold tracking-tight">{f.title}</h3>
              <p className="mt-2 text-sm leading-relaxed text-muted">{f.body}</p>
            </div>
          ))}
        </div>
      </div>
    </section>
  )
}

function Footer() {
  return (
    <footer className="border-t border-line">
      <div className="mx-auto flex max-w-6xl flex-col items-center justify-between gap-4 px-6 py-10 text-sm text-muted sm:flex-row">
        <span className="font-mono">▚ dcode-ai</span>
        <div className="flex items-center gap-6">
          <a
            href={DESKTOP_DOWNLOADS.releases}
            target="_blank"
            rel="noreferrer"
            className="transition hover:text-fg"
          >
            DCode Desktop
          </a>
          <a href={REPO} target="_blank" rel="noreferrer" className="transition hover:text-fg">
            GitHub
          </a>
          <a
            href={`${REPO}/releases`}
            target="_blank"
            rel="noreferrer"
            className="transition hover:text-fg"
          >
            Releases
          </a>
          <a
            href={`${REPO}#readme`}
            target="_blank"
            rel="noreferrer"
            className="transition hover:text-fg"
          >
            Docs
          </a>
        </div>
      </div>
    </footer>
  )
}

export default function App() {
  return (
    <>
      <Nav />
      <main>
        <Hero />
        <Desktop />
        <Features />
        <Providers />
        <Install />
      </main>
      <Footer />
    </>
  )
}
