## Learned User Preferences

- Always use Rust-native solutions; no JavaScript, Node.js, or Electron involvement
- Claude (Anthropic) and GitHub Copilot are the ONLY providers (decided 2026-10-12; all others removed 2026-10-13)
- Sign-in is OAuth only — no API-key onboarding. Claude defaults to the user's Claude account through their own signed-in `claude` binary (`[provider.anthropic] backend = "claude-code"`), used as a model only with dcode-ai's tools via a `<tool_call>` text protocol — no MCP. `backend = "api"` = Console OAuth via `ant`. Copilot uses GitHub device login. Never extract or reuse Claude.ai tokens or another product's OAuth client
- TUI redesign direction: hybrid — inline (scrollback) by default, Ctrl+\ toggles a full-screen dashboard (agents rail, transcript, diff/context panel)
- **CLI (`dcode-ai`) is the product surface**—terminal UX, JSON/NDJSON streams, and Unix-socket IPC
- Empty provider completions must fail loudly, never silently succeed
- Use plan-first workflow: create a plan document, then implement from it
- Install CLI via `cargo build --release` then `cp target/release/dcode-ai /usr/local/bin/`
- Keep each crate focused: `common` for shared types, `core` for agent logic, `runtime` for session lifecycle, `cli` for terminal UX
- User tests CLI in separate workspace directories (e.g. `test-makan`, `for-test`)
- Prefer efficient algorithms and fast execution; the CLI is intended as a spawner for heavy tasks
- Sub-agents should use isolated worktrees; parent/child session lineage should stay visible in session metadata and events

## Learned Workspace Facts

- Rust workspace with **5 crates**: `dcode-ai-common`, `dcode-ai-core`, `dcode-ai-runtime`, `dcode-ai-cli`, `dcode-ai-autoresearch`
- IPC between CLI and runtime uses Unix domain sockets (localhost TCP with a `<sid>.<port>.tcp` marker on Windows) with length-prefixed JSON frames (4-byte big-endian length, v2; see `crates/runtime/src/ipc.rs`)
- Sessions persisted as `<id>.json` (state) + `<id>.events.jsonl` (event log) in `<workspace>/.dcode-ai/sessions/`
- Master plan: `docs/plans/2026-10-12-v1-master-plan.md` (research, architecture, milestones M1–M8)
- Extensibility follows Claude Code's open file formats: `.mcp.json` (local/project/user scopes), `commands/**/*.md`, `agents/*.md`, plugins (`.claude-plugin/plugin.json` layout) installed under `~/.dcode-ai/plugins/` (`dcode-ai plugin …`)
- TUI look follows Claude Code's grammar (`>` / `⏺` / `⎿`, prompts replace the composer); default theme is `claude`
- Global config at `~/.dcode-ai/config.toml`
- Git worktrees for isolated agent runs stored at `<repo>/.dcode-ai/worktrees/<session-id>`
- **Single shipped app binary:** `dcode-ai` (CLI)
- Tokio async runtime; `async-trait` for tool executor and approval handler interfaces
- Session lineage: parent/child session IDs, inherited summary, spawn reason tracked in `SessionMeta`
- `AgentEvent` enum is the shared event bus for CLI rendering, IPC broadcast, and disk persistence
- Runtime socket dir defaults to `$XDG_RUNTIME_DIR/dcode-ai/` or `/tmp/dcode-ai/`
