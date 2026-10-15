# Changelog

## 1.0.1 — 2026-10-15

### Sessions
- `/resume` opens the session picker; `/resume <id>` jumps to a session.
- Switching sessions no longer hangs on MCP servers that ignore an unknown
  `shutdown` request (stdio servers are closed the MCP way, 3s cap each).
- A session that stopped mid-turn reopens idle, not stuck on "Thinking…".
- `-c` picks the most recent session; the picker and `-c` skip sub-agent
  sessions; sessions whose process is gone show as `ended`.
- No approve-all popup when reopening a conversation.

### Provider and model
- `/model`, `/provider`, F2 and sign-in remember your choice globally, and
  startup keeps it instead of switching to the last signed-in provider.

### Fixes
- Windows: file tools no longer reject workspace paths with 8.3 short names.
- Hooks that exit without reading their input are not reported as failed.

## 1.0.0 — 2026-10-15

First stable release: the Claude + GitHub Copilot edition. Changes since 0.0.43:

### Providers and sign-in
- Only two providers remain: **Claude** (your own signed-in `claude` binary by
  default, or Console OAuth via `ant`) and **GitHub Copilot** (device login).
  All other providers and API-key setup were removed; old configs still load.
- Copilot: native Messages API for Claude models, Responses API for codex
  models, image input on vision models.

### Terminal UI (Claude Code look and feel)
- `>` / `●` / `⎿` transcript grammar, Claude themes with a live `/theme` picker.
- Prompt input drawn with rules above and below (pink in `!` shell mode);
  approvals, `/rewind` and plan → build prompts replace the composer.
- `[Image #N]` and `[Pasted text #N +L lines]` chips, deleted as one unit.
- Shift+Tab mode cycle, Esc Esc rewind (code and conversation), `?` shortcuts,
  `/context`, `/effort`, `--plain` mode for screen readers.
- Ctrl+\ full-screen dashboard: sessions/agents rail, transcript, and a right
  rail with context, changes (per-hunk review and revert) and agents tabs
  (Alt+1/2/3, or Ctrl+D then ←/→).

### Extensibility
- Claude Code formats: `.mcp.json`, `commands/**/*.md`, `agents/*.md`, hooks,
  plugins and marketplaces with a `/plugins` browser, `/reload`.
- `dcode-ai mcp serve`, status line command, cost budget.
- Language servers chosen per project (`.lsp.json`, plugin `lspServers`).

### Automation and editors
- `dcode-ai ci` + GitHub Action (JSON result, step summary, exit codes).
- `dcode-ai acp`: Agent Client Protocol server for editors such as Zed, with
  `session/load` and image prompts.

### Safety
- `policy.toml` allow/ask/deny rules, `dcode-ai policy show|test`.
- macOS Seatbelt sandbox for shell commands (Linux uses Landlock).
- `/verify` read-only verifier, optional `verify_after_edits`.

### Tools
- `inspect_binary`: read-only static inspection of ELF/PE/Mach-O files
  (sections, symbols, imports, linked libraries, strings, cargo-auditable
  crates, Go build info).
