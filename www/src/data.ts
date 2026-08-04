export const REPO = 'https://github.com/Dhanuzh/dcode-ai'

export const INSTALL = {
  unix: 'curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | bash',
  windows: 'irm https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.ps1 | iex',
  source: 'cargo build --release && ./target/release/dcode-ai',
  local:
    'curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | DCODE_AI_INSTALL_DIR="$HOME/.local/bin" bash',
} as const

export type Feature = {
  title: string
  body: string
  /** Simple geometric glyph — avoids shipping an icon font. */
  glyph: string
}

export const FEATURES: Feature[] = [
  {
    glyph: '◍',
    title: 'Streaming reasoning',
    body: 'Watch the model think in real time, not just the final answer. Reasoning tokens stream separately and collapse when the turn ends.',
  },
  {
    glyph: '⚡',
    title: 'Visible tool execution',
    body: 'Every tool call renders with its arguments, live status and duration — plus a per-turn summary of exactly which files changed.',
  },
  {
    glyph: '▤',
    title: 'Session persistence',
    body: 'Every conversation is saved to disk. Resume, replay, or attach to any past session across restarts.',
  },
  {
    glyph: '⑂',
    title: 'Sub-agents & worktrees',
    body: 'Spawn child agents with parent/child lineage, each isolated in its own git worktree so parallel work never collides.',
  },
  {
    glyph: '⌘',
    title: 'Full-featured TUI',
    body: 'Command palette, themes, mouse support, image paste, @file mentions with workspace completion, and searchable history.',
  },
  {
    glyph: '⇄',
    title: 'Multi-provider',
    body: 'Anthropic, OpenAI, Gemini, OpenRouter, MiniMax and OpenAI-compatible endpoints. Switch provider or model inline, mid-session.',
  },
  {
    glyph: '⛨',
    title: 'Permission modes',
    body: 'Default, Plan, AcceptEdits, DontAsk or Bypass. Decide exactly how much autonomy the agent gets, per session.',
  },
  {
    glyph: '⌁',
    title: 'Headless automation',
    body: 'One-shot prompts, NDJSON streaming and JSON output make it pipe- and CI-friendly. Control detached sessions over IPC.',
  },
  {
    glyph: '◇',
    title: 'Skills & profiles',
    body: 'Auto-loads skills from AGENTS.md and skill directories. Role profiles like @build, @plan, @review shape agent behaviour.',
  },
]

export const PROVIDERS = [
  'Anthropic',
  'OpenAI',
  'Google Gemini',
  'OpenRouter',
  'MiniMax',
  'GitHub Copilot',
  'Vertex AI',
  'OpenAI-compatible',
]

/** Frames for the hero terminal — mirrors a real dcode-ai session. */
export type Frame = { kind: 'user' | 'think' | 'tool' | 'out' | 'text' | 'rule'; text: string }

export const DEMO: Frame[] = [
  { kind: 'user', text: 'add a retry with backoff to the fetch client' },
  { kind: 'think', text: 'thinking  Locating the client and its error paths…' },
  { kind: 'tool', text: 'search_code  pattern: "async fn fetch"' },
  { kind: 'out', text: 'src/net/client.rs:42' },
  { kind: 'tool', text: 'edit_file  src/net/client.rs' },
  { kind: 'out', text: '+34  -6' },
  { kind: 'text', text: 'Added exponential backoff (250ms → 8s, 5 attempts) with' },
  { kind: 'text', text: 'jitter. Transient failures now retry; 4xx still fail fast.' },
  { kind: 'rule', text: '2 tool calls · 1.4s' },
]
