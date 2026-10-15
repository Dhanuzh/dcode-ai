# M13 — MCP tool search (deferred MCP tools)

Date: 2026-10-15 · Branch: `main` · Status: ✅ done (live-tested: Claude loaded a deferred MCP tool with `tool_search select:` and called it) · Parent: [v1 master plan](2026-10-12-v1-master-plan.md)

Claude Code does not put every MCP tool schema into every request when
there are many: they are *deferred* and the model loads the ones it needs
with a tool search. A single server (e.g. Power Automate, 22 tools) can
otherwise dominate the prompt.

## Design
- `[mcp] tool_search_threshold` (default 15; 0 = never defer). When the
  session loads more MCP tools than that, all MCP tools are deferred.
- `ToolRegistry` keeps deferred tools executable but leaves them out of
  `definitions()` until activated (shared activation set).
- New read-only `tool_search` tool: `query` = keywords, or
  `select:name1,name2` for exact names. It returns the matching tools'
  full schemas and activates them, so they are sent from the next request
  on. Its description lists the deferred tool names so the model knows
  what exists.
- The TUI and `/mcp` keep showing every server and tool.

## Verification
Unit: registry hides/reveals deferred tools; search by keywords and
`select:`; threshold. Live: Claude finds and calls a deferred MCP tool.
