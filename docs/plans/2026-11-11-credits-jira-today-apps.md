# Plan — fewer credits, Jira check and daily summary, app control (2026-11-11)

## 1. Fewer credits

### 1a. Keep the prompt cache intact (verified in code)
Tools render first in the cached prefix (tools → system → messages): any
change to the tool list makes the next request a full cache miss.
Today the list changes when:
- MCP servers that connect after the 3 s startup wait are registered "at
  the start of the next turn" (`adopt_ready_mcp`) — new tools appear;
- `defer_behind_search` rebuilds `tool_search`, whose description lists
  every deferred tool — its definition changes;
- a tool activated by `tool_search` shows up *in its registration
  position* (mid-list), not at the end.

Fix:
- Late MCP tools are **always deferred** (never added straight to the
  list) and the `tool_search` definition stays **frozen** after the first
  request; late tools are announced in the conversation (a one-line
  note on the next user turn: "New tools via tool_search: …").
- Activated tools go **at the end**, in activation order — one change per
  activated tool, earlier tools untouched.
- Anthropic API backend: use Anthropic's server-side tool search
  (`tool_search_tool_bm25_20251119` + `defer_loading: true` on deferred
  tools) so the tool list never changes. Copilot and the `claude` binary
  keep the client-side search.
- A **cache-break warning**: when a turn reads <30% from cache after turns
  that read >70%, a dim note says so and why if known ("tool list
  changed: 3 MCP tools added").

### 1b. Usage history for `/cost`
`/cost` already shows this session's fresh / cache-read / cache-write /
output split and hit rate. Missing: history and sub-agents.
- `usage.jsonl` lines gain `input`, `output`, `cache_read`, `cache_write`,
  `cost_usd`, `session`, `parent` (old lines still read).
- `/cost today|week|month`: by day and by model; cache-hit rate; top
  sessions; sub-agents under their parent.

### 1c. Sub-agent budgets and a stop key
- `[subagents] max_tokens = 400_000`, `max_minutes = 20` (0 = no limit),
  per spawn overridable by the agent (`budget_tokens`, never above the
  config). The child's `CostUpdated` events are watched; past the budget
  its cancel flag is set and it returns what it has with
  `status: "budget"`.
- Dashboard agents tab: `x` stops the selected sub-agent (asks once).

## 2. Jira

### 2a. `/board jira doctor` — a live check, read-only
✓/✗ checklist against the real site: MCP server reachable and signed in;
tools present (search, create, edit, transitions, comments; worklogs and
attachments via `executeRead/Write` + `discover`); who you are
(`atlassianUserInfo`); the site and cloud id; the project readable; the
board's pull JQL with its ticket count; one ticket's transitions and
required fields. Each ✗ says how to fix it. Nothing is written to Jira.

### 2b. `/board jira today` — the daily summary (no model, no credits)
From the board after a sync: what changed since yesterday (others'
comments and moves), new to you, due today / overdue, in progress,
waiting on you (newest comment isn't yours), blocked, done yesterday.
`[board] daily_summary = true` shows it once a day on the first session
start. The agent can be asked to turn it into a stand-up note.

## 3. App control (from `docs/plans/2026-11-11-app-control.md`)
Defaults chosen for the open questions:
- Apps: any Electron / WebView2 app by path; known names for VS Code,
  Slack, Teams, Outlook (new), Postman, Discord, Notion. UIA: any window,
  tested against Notepad.
- The Windows helper is downloaded only after asking, each time it's
  missing.

Order: **A** (`/app open`, named CDP connections, `target` on `browser`)
in this batch; **B** (UIA helper, Windows-only, built and tested in the
Windows CI job) next.

## Order of work
1a → 1b → 1c → 2a → 2b → 3A; release after 1 and after 2+3A.

## Built (v1.0.59)
- 1a without Anthropic's server-side tool search: it needs the search
  result blocks kept in the conversation, which dcode-ai's message model
  doesn't store yet — the client-side fixes cover the cache breaks found.
- 1c budget counts every token processed (cache reads included), so the
  default is `subagent_max_tokens = 2_000_000` (≈20 steps at 100k), not
  400k; the stop key is `x` in the sub-agents list (background agents;
  Esc stops a foreground one with the turn).
- 2a, 2b as planned (`/board jira today on|off`, marker
  `.dcode-ai/board/today-shown`).
- 3A next.

