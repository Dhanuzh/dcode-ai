# M20 — `/mcp` panel with in-session sign-in, more plugin marketplaces, resume fix (v1.0.19)

Status: implemented

References: Claude Code's `/mcp` (server list → server menu → Authenticate;
the sign-in screen shows the URL with "c to copy", waits for the browser,
then "Authentication successful. Connected to <server>.") and its
`/plugin` Discover tab.

## 1. Resume fix (found from a session log)

After `/resume` (and `/project`), approvals, question answers and Esc still
went to the previous session's maps: every approval waited the 5-minute
timeout and came back "denied", questions hung, Esc did nothing.
`take_ipc_approval_pending()` returned `None` after resume because
`resume_in_process` had already moved the map out. Fix: one
`attach_runtime_to_tui` helper used at start, `/resume` and `/project`;
the answer dispatchers look the live maps up; the TUI's cancel flag and
steer inbox are handed to the resumed agent.

## 2. `/mcp` panel (TUI)

- `/mcp` opens a panel in place of the old info box: one row per server
  with status — `✔ connected · N tools`, `! needs sign-in`, `✗ failed`,
  `○ disabled`, `… connecting` — kind (http/stdio) and origin.
- Enter on a server opens its menu: **Authenticate** (remote servers),
  **Reconnect**, **Sign out** (when signed in), **Enable/Disable**.
- **Authenticate** runs the OAuth flow inside the session (same code as
  `dcode-ai mcp login`, split into `begin` / `finish`): opens the browser
  and shows the sign-in URL with **c to copy**, waits for the loopback
  redirect, and accepts a pasted redirect URL when the browser can't reach
  localhost (WSL). On success the server reconnects at once and its tools
  join the session: "Authentication successful. Connected to <name>."
- Reconnect = disconnect, load again, register tools (deferred behind
  `tool_search` past the threshold), emit `McpStartupUpdate`.
- The startup failure line points at `/mcp` instead of a shell command.

## 3. Plugins: more marketplaces

Claude Code's Discover also shows the "Anthropic Directory", which is the
claude.ai catalog fetched with the user's claude.ai sign-in — off limits
(dcode-ai never reuses Claude.ai tokens). The public equivalents are
Anthropic's GitHub marketplaces. `/plugins` now adds, once each (a removed
one stays removed): `anthropics/claude-plugins-official` (~315),
`anthropics/claude-plugins-community` (`claude-community`, ~2,280) and
`anthropics/knowledge-work-plugins` (~120).
