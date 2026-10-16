# Changelog

## 1.0.6 — 2026-10-16

### Fixes
- Claude: protocol examples no longer break or cut a turn. `<tool_call>`
  inside a ``` code block, a placeholder like `<tool_call>{json}</tool_call>`,
  and a backtick-quoted tag split across stream chunks are all plain text.
- A tool call Claude writes but that cannot be parsed (or is never closed)
  is returned to it as a failed `invalid_tool_call` result so it can retry,
  instead of failing the turn.

## 1.0.5 — 2026-10-15

### New (Claude Code parity)
- **MCP tool search**: with more than 15 MCP tools (`[mcp]
  tool_search_threshold`), their schemas are loaded on demand through
  `tool_search` instead of being sent with every request.
- `/release-notes` shows what's new; `/todos` shows the agent's task list.
- Scripting flags like Claude Code: `--append-system-prompt <text>`,
  `--allowed-tools` and `--disallowed-tools` (Claude names such as `Read`,
  `Edit`, `Bash(git:*)`, or dcode-ai rules such as `execute_bash:cargo *`).

## 1.0.4 — 2026-10-15

### New (Claude Code parity)
- **Ctrl+B** moves a running shell command to the background; it keeps
  running, shows in the footer and `/bashes`, and the agent is notified
  when it finishes. Running commands show `(ctrl+b to run in background)`.
- **Thinking keywords**: `think hard`, `think harder` / `megathink` and
  `ultrathink` raise reasoning effort for that message (highlighted as you
  type).
- **Output styles**: `/output-style` with Default, Explanatory (★ Insight
  notes) and Learning (`TODO(human)`), plus custom `output-styles/*.md`.
- **Session cleanup**: sessions untouched for 30 days are deleted at startup
  (`[session] cleanup_period_days`, 0 = keep); `dcode-ai sessions --prune
  <DAYS>`.

## 1.0.3 — 2026-10-15

### Background shells (Claude Code style)
- `run_background` jobs show in the footer (`· 1 shell`), turns end with
  `✻ Crunched for 1m 9s · 1 shell still running`, and a finished job prints
  `● Background command "…" completed (exit code 0)` and wakes the agent with
  its exit code and last output.
- `/bashes` lists background jobs; `/bashes kill <id>` stops one.

### Changes
- `bypass-permissions` runs shell commands without asking (destructive
  commands are still refused).

## 1.0.2 — 2026-10-15

### Fixes
- Claude: writing about the `<tool_call>` protocol (in backticks, or a bare
  tag followed by prose) no longer fails the turn with "unterminated tool
  call"; a quoted `<tool_result>` no longer cuts the reply short.

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
