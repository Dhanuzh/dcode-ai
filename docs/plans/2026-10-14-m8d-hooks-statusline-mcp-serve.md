# M8d — Turn hooks, status line, MCP server mode, cost budget

Date: 2026-10-14 · Branch: `feat/claude-copilot` · Status: ✅ done, live-tested · Parent: [v1 master plan](2026-10-12-v1-master-plan.md)

Chosen autonomously ("you think, do what's best for dcode") after M8a–c.

1. **Turn hooks.** New hook events `user_prompt_submit` (before a prompt
   reaches the model; a blocking hook that exits non-zero stops the turn and
   shows its stderr) and `stop` (after every turn). Plugin `hooks.json`
   maps `UserPromptSubmit` → `user_prompt_submit`, `Stop` → `stop` (was
   session end). Payload: `{session_id, prompt}` / `{session_id, status}`.
2. **Status line.** `[ui] statusline_command = "…"` runs a shell command
   (JSON session info on stdin: model, cwd, session id, permission mode,
   tokens, cost) at most once per second off the UI thread; its first
   output line replaces the footer's right side.
3. **MCP server mode.** `dcode-ai mcp serve` speaks MCP over stdio
   (initialize, tools/list, tools/call) and exposes dcode-ai's file,
   search, edit and shell tools for the current directory, so Claude Code,
   Copilot CLI or any MCP client can use them. Read-only unless
   `--allow-writes`.
4. **Cost budget.** `[session] max_cost_usd` stops a session's agent loop
   with a clear error once the estimated cost reaches the limit (`0` = off).

Verification: unit tests per item; `mcp serve` exercised end-to-end with a
scripted JSON-RPC client; live TUI check of status line and hooks.
