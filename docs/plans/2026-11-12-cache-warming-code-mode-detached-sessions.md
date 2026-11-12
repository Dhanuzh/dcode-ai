# Plan — cache warming, Code Mode, sessions that outlive the terminal (2026-11-12)

Ideas from Pi 1.0 (cache warming, Codemode) and OpenCode 2.0 (Code Mode,
one background service per session). Order: A (small) → B → C (largest).

**Status (v1.0.61).** Built: D (herdr status + tab titles), E
(`browser_task`), A (keep-warm while idle; warm-at-start not yet), F
(`/schedule`; runs read-only or `--auto`, no worktree yet), B (`run_script`
on Boa; inner calls limited to what the mode allows without asking, rather
than prompting mid-script). Next: C (sessions that outlive the terminal),
its own release.

## A. Prompt-cache warming

**Why.** The provider caches the prompt prefix for ~5 minutes. After a pause
the next turn re-sends everything at full price; idle compaction (v1.0.57)
shrinks it but costs a summary and loses detail. A warm-up request reads the
prefix at cache price (Opus 5.5: $0.20/MTok — ~4¢ for 200k) and resets the
timer, so the next real turn stays cheap and nothing is summarized.

**What.**
1. *Keep-warm while you're around*: idle 4 min 30 s on the Anthropic API
   backend → one warm-up request (the conversation as it stands, `max_tokens`
   0 — exact shape from the prompt-caching guide's pre-warming section).
   Up to `keep_warm_max = 3` times per idle stretch (~15 min), and only while
   the terminal is focused or you typed in the last 10 min. Then idle
   compaction takes over as today.
2. *Warm at start/resume*: right after a session starts or resumes, warm the
   system prompt + tools, so the first turn's fixed part is already cached.
3. `[memory.context] keep_warm = true`, `keep_warm_max = 3`; off for Copilot
   (its caching isn't ours to control) and the `claude-code` backend (the
   `claude` binary manages its own).
4. `/cost` counts warm-ups separately ("keep-warm: 3 × ~4¢"), so the saving
   is visible.

**Tests.** Scheduler decisions (idle times, focus, max, backend); request
shape against a fake provider; usage logged as warm-up.

## B. Code Mode (`run_script`)

**Why.** Multi-step tool work (search Jira, read 10 tickets, keep the overdue
ones) costs one model round-trip per call, and every intermediate result
enters the context. A script chains the calls in one step; only its final
result comes back.

**Engine.** [Boa](https://github.com/boa-dev/boa), a JavaScript engine in
pure Rust (AGENTS.md: Rust-native, no Node/JS runtimes). Models write
JavaScript fluently; Boa keeps the build all-Rust. (Fallback: Rhai.)

**What.**
- Tool `run_script {code}`. Inside: `tools.read_file({path})`,
  `tools.search(...)`, … and `mcp.<server>.<tool>({...})` — every tool the
  session has, as synchronous functions returning JSON. `print()` lines and
  the script's return value are the tool result (capped at 20k chars).
- Each inner call goes through the normal path: approvals and permission
  mode, auto-mode checks, undo checkpoints, hooks, the event log (shown
  collapsed under the script in the transcript).
- Sandbox: no file, network or process access except through tools; limits
  of 60 s, 50 tool calls, 64 MB, 20k chars out. Errors come back with line
  numbers, so the model fixes the script.
- Boa is synchronous: the script runs on a blocking thread; host functions
  send each call to the agent's async tool executor and wait for the answer.
- Offered when useful: the tool description says when to script (3+ chained
  calls, filtering big results); MCP servers listed as script functions.
  `[harness] code_mode = true` (default), `false` to measure the difference.

**Measure before keeping it on.** The same Jira task with and without
(`/cost today`). A July 2026 study found token reduction doesn't always mean
a lower bill because caching already discounts repeated context; keep it on
only if `/cost` shows the saving.

**Tests.** Script calling fake tools; approval asked for a mutating inner
call; limits (time, calls, output); error with line number; MCP functions.

## C. Sessions that outlive the terminal

**Today.** The interactive session (agent, tools, slash commands) lives in
the TUI process; closing the terminal ends it (it's saved, `/resume` brings
it back). The pieces for more exist: `dcode-ai serve` (a headless session on
a socket), `spawn` (start one in the background), `join` (the full TUI as a
guest of a running session, sending messages), `web` (browser UI),
`/invite web` (phone link), `remote --keep` (tmux over SSH).

**Target.** Each interactive session runs in a small background *host*
process; the TUI is a client of it. Close the terminal → the session keeps
going (a running turn finishes, background agents carry on); open dcode-ai
again → reattach where you were — or from a browser tab or your phone.

**What.**
1. *Host*: `dcode-ai` starts the session host detached (`setsid` on Unix,
   a detached process on Windows) — the REPL loop, runtime and slash
   commands move there unchanged; it already speaks IPC.
2. *Owner client*: the TUI attaches as **owner** — the guest client
   (`tui/guest.rs`, the same full UI) plus everything only the owner may do:
   approvals, all slash commands, file pickers, `/board`, `/app`. Commands
   and keystrokes go over the socket; display state streams back.
3. *Detach*: closing the terminal or `/detach` leaves the host running.
   With no client and nothing running, a host exits after
   `[session] idle_exit_hours = 12` (state and checkpoints are on disk).
4. *Reattach*: starting `dcode-ai` in a folder with a running session asks
   "1 session running in the background — attach?"; `dcode-ai attach <id>
   --tui`; `/resume` marks running sessions with a live dot.
5. *Elsewhere*: `dcode-ai web --attach <id>` for a browser tab;
   `/invite web --owner` for your phone (owner rights behind the invite key,
   approvals included).
6. *While detached*: a turn that needs an approval waits for you; you get a
   desktop notification. `dcode-ai status` lists hosts and what they wait on.

**Phases.** C1 host + local owner attach; C2 detach on close, reattach
prompt, live sessions in `/resume`; C3 browser/phone as owner.

**Risks.** The REPL/TUI split is the biggest refactor yet: anything the TUI
does locally today (clipboard, file pickers, `/board` overlay, opening
links) must work from the client side. C1 starts by listing every place the
REPL touches the terminal directly.

## D. Show status in herdr (and other terminal managers)

**Problem.** In [herdr](https://herdr.dev) the sidebar shows Claude Code as
working (yellow dot, "claude") but dcode-ai panes as an unknown, idle agent:
herdr only knows the agents that report to it (or that it has screen rules
for), and dcode-ai does neither. Its terminal title is also static
(`dcode-ai: <folder>`), where Claude's changes while it works.

**How herdr does it** (docs: "Adding herdr support to an agent"). Inside a
herdr pane every process gets `HERDR_ENV=1`, `HERDR_PANE_ID`,
`HERDR_BIN_PATH`, `HERDR_SOCKET_PATH`. An agent reports itself:

```
"$HERDR_BIN_PATH" pane report-agent "$HERDR_PANE_ID" \
  --source dcode-ai --agent dcode-ai --state working|idle|blocked \
  --seq <ms timestamp> [--message "needs approval: edit_file"] \
  [--agent-session-id <id> -- dcode-ai --resume <id>]
"$HERDR_BIN_PATH" pane report-metadata "$HERDR_PANE_ID" \
  --source dcode-ai --agent dcode-ai --title "<session name / task>"
"$HERDR_BIN_PATH" pane release-agent "$HERDR_PANE_ID" \
  --source dcode-ai --agent dcode-ai --seq <ms>
```

**What.** Built into dcode-ai (no `herdr integration install` needed):
- Only when `HERDR_ENV=1` and the variables are set; otherwise nothing.
- `working` when a turn starts (and while background agents run),
  `idle` when it's ready for input, `blocked` with a message when an
  approval or a question waits — from the TUI's busy/approval state.
- On start and on `/resume`/`/new`: `--agent-session-id <id> -- dcode-ai
  --resume <id>`, so herdr's session restore reopens the same conversation.
- `report-metadata --title` with the session's name (or first prompt) so
  the sidebar shows what it's working on.
- `release-agent` on a real quit (not on `/new` or a model switch).
- Reports run in the background with a short timeout; only the latest state
  is sent (older queued ones dropped); failures ignored.
- Also for every terminal: a terminal title that follows the state
  (`✳ dcode-ai · <task>` while working, `● … needs you` when blocked), which
  tmux, WezTerm and Windows Terminal tabs show too.

**Tests.** State changes → reports (a fake herdr binary records its args);
nothing outside herdr; seq increases; resume argv valid (plain command, no
apostrophes, ≤ 64 args).

**Order.** D is small and independent: build it first, then A → B → C.

## E. Browser and app work in a sub-agent

**Why.** Every `browser` step returns page text, numbered elements or a
screenshot — often 3–6k tokens each — and all of it stays in the main
conversation. A ten-step web or app task can add 50k tokens that are never
needed again, and every later turn re-sends them.

**What.** A tool `browser_task {goal, target?, url?}`:
- Starts a sub-agent (the existing child-session machinery) whose only tools
  are `browser` (any `target` — the browser or a connected desktop app) and
  read-only file tools, with no worktree (it doesn't edit the project).
- It works through the goal on its own and returns a short report: what it
  did, what it found (the data asked for), what's on screen now, and the
  paths of any screenshots it saved. Only that report enters the main
  conversation.
- Same rules as any sub-agent: approvals bridged to you, the token and time
  budget (`[harness] subagent_max_tokens` / `subagent_max_minutes`), `x` to
  stop it, its steps visible in the agents list.
- Model: the session's model by default; `[harness] browser_task_model` can
  set a cheaper one for routine clicking.
- The page or app stays open and connected afterwards, so the main agent (or
  a follow-up `browser_task`) can continue where it left off.
- The tool description steers the choice: multi-step browsing or app work
  (logins, forms, collecting data across pages, testing a flow) →
  `browser_task`; one quick look or click → `browser` directly.

**Measure.** The same web task done directly and through `browser_task`:
`/cost` and the context size afterwards (`/context`).

**Tests.** A child session limited to the browser tools; the report returned
and nothing else; budget and stop apply; approvals reach the parent; the
connection survives the task.

**Order.** After D, before A — or alongside B, since both cut the context
that tool-heavy work leaves behind.

## F. Scheduled runs

**Why.** Recurring work — the Jira day and a stand-up draft every morning, a
nightly test run, a weekly dependency check — today needs you to start it.
Codex (automations), Copilot CLI (prompt scheduling) and Droid (interval
loops) run such prompts on a schedule.

**What.**
- `/schedule add "<when>" <prompt>`: `<when>` in plain forms — `daily 09:00`,
  `weekdays 09:00`, `mon 18:30`, `every 2h`, or a cron expression
  (`0 9 * * 1-5`). `/schedule list`, `/schedule rm <n>`, `/schedule run <n>`
  (now), `/schedule pause|resume <n>`.
- Stored per project in `.dcode-ai/schedules.json` (next run, last run, last
  result).
- Each run is a background session (the `spawn` machinery) in its own
  worktree when it may change files (`--worktree`, default for prompts that
  aren't read-only), with the sub-agent budget, in accept-edits mode — never
  bypassing approvals: a run that needs one waits and notifies you.
- Runs while dcode-ai is open in that project (the TUI checks every minute);
  a run missed while it was closed runs once on the next start (not every
  missed one). A run never overlaps itself.
- Results: a desktop notification, a line in the transcript, and the session
  in `/resume` (marked unread until opened).

**Tests.** Parsing `<when>` forms and the next time (incl. weekdays across a
weekend, DST-safe in local time); missed runs collapse to one; no overlap;
store round-trip.

## Questions
- Engine for Code Mode: Boa (pure-Rust JavaScript, recommended) or Rhai
  (Rust scripting language, less familiar to models)?
- Keep-warm on by default (≈4¢ a ping on Opus 5.5 at 200k), up to 3 pings?
- Host lifetime when detached and idle: 12 h?
