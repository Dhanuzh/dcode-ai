# M8a — Plugins, MCP config, session resume, loose ends

Date: 2026-10-14 · Branch: `feat/claude-copilot` · Status: ✅ done, live-tested (plugin command, plugin MCP tool + hook, `mcp add/list/get/remove`) · Parent: [v1 master plan](2026-10-12-v1-master-plan.md)

User request: "complete the pending things and work on plugins, mcp, sessions
resume, other features also". Formats follow Claude Code's open file layouts
where one exists, so existing `.mcp.json` files, `commands/*.md` and Claude
Code plugins work unchanged. No Claude Code code or assets are copied; only
file formats are read.

## What already exists (not rebuilt)
- MCP client: stdio + Streamable HTTP, `[[mcp.servers]]` in TOML, `/mcp`,
  `/mcp test`, `dcode-ai mcp` (list).
- Skills (`SKILL.md`) + command files (`commands/*.md`, `$ARGUMENTS`) from
  `~/.dcode-ai`, `~/.claude`, `.dcode-ai`, `.claude`; `dcode-ai skills add`.
- Hooks (`[hooks]`), memory store, `-r/--resume` (last session),
  `dcode-ai resume <id>`, TUI session picker, event-log replay.

## Scope

### 1. MCP configuration (Claude-compatible)
- Read `.mcp.json` (project), `.dcode-ai/mcp.json` (local) and
  `~/.dcode-ai/mcp.json` (user): `{"mcpServers": {name: {command, args, env}
  | {type: "http"|"sse", url, headers}}}`. `${VAR}` / `${VAR:-default}`
  expansion. TOML `[[mcp.servers]]` wins on name clashes; then local >
  project > user > plugins.
- `dcode-ai mcp list|get <name>|add <name> [--scope local|project|user]
  [--transport stdio|http] [-e K=V] [-H 'K: V'] -- <command or url> [args…]
  |remove <name> [--scope]` (bare `dcode-ai mcp` still lists).
- `/mcp` shows each server's source and status.

### 2. Plugins
- Layout (Claude Code compatible): `<plugin>/.claude-plugin/plugin.json`
  (or `.dcode-plugin/plugin.json`, or `plugin.json`) with
  `name, version, description`; contributes `commands/**/*.md`
  (→ `/plugin:command`, subdirs → `/plugin:dir:command`),
  `skills/*/SKILL.md`, `.mcp.json` / manifest `mcpServers`
  (`${CLAUDE_PLUGIN_ROOT}` / `${DCODE_PLUGIN_ROOT}` expanded), and
  `hooks/hooks.json` (`PreToolUse`, `PostToolUse`, `SessionStart`,
  `SessionEnd`, `Stop`→session end; `command` hooks only).
- Installed under `~/.dcode-ai/plugins/<name>/`, registry
  `~/.dcode-ai/plugins/installed.json` (source, version, commit, enabled).
- `dcode-ai plugin install <path | owner/repo | git url> [--plugin name]`
  (a repo with `.claude-plugin/marketplace.json` installs the named
  plugin, relative or GitHub sources), `list`, `remove`, `enable`,
  `disable`, `update [name]`.
- TUI `/plugins` lists plugins and what each contributes;
  `/plugins enable|disable <name>`.

### 3. Sessions
- `-c/--continue` resumes the last session (alias of today's `-r`);
  `--resume <id>` resumes a specific one; `-r` with no id keeps working.
- Rewinds survive resume: new `AgentEvent::ConversationRewound
  { user_messages_dropped }` is logged and applied on replay (the live TUI
  already truncated, so it applies only while replaying).
- Resumed sessions keep their model (stored in session meta).

### 4. Other features / pending items
- `# note` in the composer saves a memory note (Claude's `#` shortcut).
- Copilot/OpenAI-compatible streams: malformed tool-call JSON fails loudly
  instead of being passed on as `{"raw": …}` (matches the Claude path).
- Command files in sub-directories become namespaced commands.

## Verification
Unit tests: `.mcp.json` parse + precedence + env expansion; `mcp add/remove`
round-trip per scope; plugin manifest discovery, command namespacing,
plugin MCP/hook contribution, marketplace resolution, install/remove from a
local path; rewind replay; `#` note; Copilot invalid tool JSON. Live: install
a local plugin, run its command, MCP server from `.mcp.json` via `/mcp test`,
`dcode-ai -c` after a rewind.
