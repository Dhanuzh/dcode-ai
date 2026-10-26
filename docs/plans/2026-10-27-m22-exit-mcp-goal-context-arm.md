# M22 — resume hint, MCP on/off + token refresh, goal on resume, nested instructions, context warning, ranged reads, Homebrew + Linux ARM (v1.0.21)

Status: implemented

## 1. Resume hint on exit
After the TUI closes, a session with at least one user message prints
`Resume this session with: dcode-ai --resume <id>` (Claude Code prints the
same line).

## 2. MCP on/off is remembered
Turning a server off in `/mcp` is stored per folder in
`~/.dcode-ai/folders.json` (`disabled_mcp`), next to the remembered
permission mode, and applied when the config loads. Turning it on removes
it. Nothing is written to the project.

## 3. MCP token refresh
Refresh on expiry and on 401 already existed. Gap: two dcode-ai sessions
share `mcp-auth.json`; when one refreshes (and the server rotates the
refresh token) the other kept the old one, its refresh failed, and it
reported "needs sign-in". Now a refresh first reloads the store and adopts
a newer token if another session saved one, and after a failed refresh
checks the store again before giving up.

## 4. `/goal` survives `/resume`
`SessionMeta.goal` (objective, status, budget, tokens at start, turns) is
saved with the session. On resume an active goal comes back paused, shown
in the footer, with a note: `/goal resume` continues it.

## 5. Nested CLAUDE.md / AGENTS.md
When a tool touches a path in a subfolder (`path` / `file_path` input),
`AGENTS.md`, `CLAUDE.md` and `CLAUDE.local.md` in the folders between the
workspace root and that path are appended to the tool result once per
session, wrapped as instructions that apply to that folder (Claude Code's
nested memory). Capped at 8k chars each.

## 6. Context warning
Footer right side: `N% context left` from 50% (as before); from 80% used it
turns into a warning `context 85% used · /compact` (red from 95%).

## 7. Cheaper reads
`read_file` takes `offset` (1-based first line) and `limit` (lines). With
no range, a big file returns its first lines up to ~20k chars with a
footer naming the total and how to read on, instead of the agent-level
middle elision. The tool description tells the model to locate with
`search_code` and read only the part it needs.

## 8. Homebrew + Linux ARM
- Release matrix gains `aarch64-unknown-linux-gnu` on GitHub's
  `ubuntu-24.04-arm` runner. `install.sh`, `dcode-ai update` and the
  formula know it.
- The release job commits the filled formula to `Formula/dcode-ai.rb` on
  `main`, so the repository is its own tap:
  `brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai && brew install dcode-ai`.
