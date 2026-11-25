# Backlog — round 5 (after v1.0.81)

**Status (2026-11-19):** written, not yet compiled (no local builds; CI
will build and test): fixes 1–5 (24 skipped — `crates/web-api` holds an
untracked `.env`), U1–U11. Next: TUI 6–9, video 11–15, talk 16–18, agent
19–22, code health 23, 25–28.

Planned 2026-11-18. Nothing here is built yet; each item says what is wrong
or missing, the fix, and how to check it.

## 1. Fixes (do first)

1. **Stray markup shown as reply text** — the transcript shows
   `● <retrieval_document>` and `● </retrieval_document>` around a tool call
   (seen with claude-opus-5-5 on the claude-code backend). The model wraps
   tool output in its own tags and the text protocol passes them through as
   visible text.
   *Fix:* in the `<tool_call>` splitter (`provider/claude_code.rs`) drop
   known wrapper tags — `<retrieval_document>`, `<function_results>`,
   `<document>`, `<search_results>` and their closing tags — from visible
   text (not inside code fences or backticks); a reply block that is empty
   after stripping isn't shown. *Check:* splitter tests with the tags split
   across deltas; the existing chunk-invariance fuzz test covers splits.
2. **Builds fill the disk** — two build folders reached ~160 GB (repo
   `target/` 62 GB, `~/.cache/dcode-ai-target` 97 GB; 820 crates in the
   tree). Causes: two folders building the same thing, and `[profile.dev]`
   keeping full debug info for every crate.
   *Fix:*
   - `[profile.dev] debug = "line-tables-only"` and
     `[profile.dev.package."*"] debug = false` (dependencies without debug
     info) — usually cuts the folder by more than half and speeds linking;
     backtraces keep file:line for our own code.
   - One shared build folder (`.cargo/config.toml` `target-dir`) for the
     hook, the editor and manual builds.
   - Delete old build files automatically: artifacts unused for 7 days
     (a small `scripts/sweep.sh` the hook runs, or `cargo sweep -t 7`).
   *Check:* `du -sh` of the folder after a full build, before and after.
3. **The commit hook takes ~10 minutes** — cargo-husky runs fmt, clippy and
   the whole test suite on every commit. *Fix:* the hook runs fmt + clippy
   + tests of the crates the commit touches; the full suite stays in CI
   (Linux, macOS, Windows, live video).
4. **Releases need lockfile fix-ups** — `Cargo.lock` misses the version
   bump. *Fix:* `scripts/release.sh <version>` — bump, update the lock
   entries, CHANGELOG heading, commit, tag, push.
5. **Tests read the real home folder** — a talk test failed on the user's
   `voice-persona`. *Fix:* tests that touch `~/.dcode-ai` run with a
   temporary home.

### 1b. Reported 2026-11-22

- **B1 (done v1.0.91). Broken pipe panics** — `dcode-ai plugin search rust | head -8`
  crashes with "failed printing to stdout: Broken pipe" (`println!`
  panics when the reader closes early). Seen while recording the demo.
  *Fix:* subcommand output goes through a writer that treats `EPIPE` as
  "stop quietly, exit 0" (not a global `SIGPIPE` reset — the runtime
  writes to hook/MCP pipes and must keep getting `EPIPE` errors there).
  *Check:* `dcode-ai plugin search rust | head -1` exits 0 with no
  message; a test pipes a few subcommands into a closed reader.

## 2. TUI

6. Search this conversation from Ctrl+P (`?`), jump to the message.
7. Clickable `path:line` in replies opens the editor (VS Code, JetBrains,
   `$EDITOR`).
8. `/recap` opens the sharp PNG instead of the blurry half-block preview.
9. `/rewind` shows the diff that would come back before confirming.
10. Collapse long tool output by default in the inline view (Ctrl+O shows
    all), like the dashboard.

### UI picks (chosen 2026-11-18)

*Status 2026-11-25:* TUI 10 and U1–U11 all shipped (v1.0.85–1.0.86;
U1 agents-tab wrap and U11 `/diff open` finished in v1.0.92).

- **U1. Clipped right panel** — dashboard right rail cuts long lines
  mid-word ("ddhanus…", "arepoint"). Wrap text in rail panels (word wrap,
  `Paragraph::wrap`), or a readable one-line summary with Enter to expand.
- **U2. Status bar cut off** — "claude-opus-" loses its end when the mode
  text is long. Short model names (`opus-5.5`, `sonnet-5.5`), and parts
  dropped by priority (hint text first, model last) to fit the width.
- **U3. Collapse long tool output inline** — first and last few lines plus
  "+120 lines · Ctrl+O", as the dashboard already does.
- **U4. Better code blocks** — language label, a key to copy the block,
  line numbers on blocks over ~15 lines.
- **U5. Resizable dashboard panes** — dragging the rails exists
  (`dash_hits.left_w/right_w`); add Alt+←/→ (and Alt+Shift for the left
  rail) and remember the widths in `~/.dcode-ai/tui.json`.
- **U6. Choose the dashboard panels** — show/hide plan, agents, files,
  office, log rails (`/dashboard panels` or a picker), remembered.
- **U7. Full-screen panel** — one key zooms the focused panel (diff,
  transcript, files) to the whole screen and back.
- **U8. Jump between turns** — `[` / `]` move to the previous / next prompt
  in the transcript (scroll view), `g` / `G` to the top / bottom.
- **U9. Side-by-side diff** — when the terminal is wide (≥ 140 columns) the
  diff panel shows old and new side by side; narrow stays unified.
- **U10. Readable line length** — on very wide terminals, prose in replies
  wraps at ~100 columns (code blocks and tables keep their width).
- **U11. Pop-out panel** — open the diff or files panel as its own
  full view (`/diff open`, `/files open`), Esc returns.

## 3. Video

11. Keystroke badges (`Ctrl+\`, `Shift+Tab` shown as pressed) and callouts
    (arrow/box/label for a few seconds) for screen recordings.
12. Privacy blur for whole-screen recordings (on-screen text needs OCR;
    start with blurring a list of screen regions from the edit list).
13. Fades between cut segments, not only around cards.
14. `/video fix`: the agent shortens narration or slows footage for what
    `/video check` flags.
15. Interactive timeline: pick a line or zoom and move it with the keys.

## 4. Talk

16. Real-microphone checklist for `wake`, `ptt`, `lang`.
17. `/talk model small` — switch to the bigger multilingual speech model
    (Tamil and similar), downloaded only when asked.
18. Piper voices picked automatically for the reply's language when one is
    installed (already for German; list more in `/talk voice`).

## 5. Agent and code

19. `/review` before commit — staged changes checked for bugs, secrets,
    missing tests.
20. Overnight queue — tasks run in worktrees, a morning summary.
21. Reproduce from an issue — a failing test first from a Jira/GitHub bug,
    then the fix.
22. Replace `chrono` with `jiff` (unmaintained advisory ignored for now).

## 5b. Multi-agent (from the Antigravity comparison, 2026-11-19)

- **A1. Swarm board** — sub-agents of a `/fleet` (or any parent) post to
  and read from one shared board: claims ("taking: auth tests"), findings,
  questions, done. A `board_post` / `board_read` tool pair; the head agent
  and every worker see new posts between steps. Live view in the dashboard
  (a rail tab, Alt+8) with who posted what and when; `/fleet board` inline.
  *Check:* two sub-agents on a split task avoid duplicate work by claiming
  on the board (test with the mock provider).
- **A2. Workflow fan-out** — a `run_workflow` tool (or `run_script` mode
  `workflow: true`): a script maps a list of items to sub-agent tasks
  (`agents.run(task, {worktree})`), runs N at a time, gathers their
  reports; no 60 s cap (hours, with a budget), progress shown live
  ("12/40 done · 2 failed · ~6 min left") in the dashboard and inline,
  stoppable with Esc, resumable. Approvals still apply to the workers.
  *Check:* a 20-item fan-out with a mock provider finishes, reports per
  item, respects the parallel limit and the budget.

## 5c. From the Codex comparison (2026-11-19)

- **W1. Split terminal workspace** — several conversations side by side in
  one terminal: panes (split right/below) and tabs, each a session or a
  side chat (`/btw` as a pane instead of a popup). A leader key (Ctrl+X by
  default, `/keymap`) then a letter: `|` split right, `-` split below,
  `t` new tab, arrows move focus, `x` close. Layout saved per project.
  Builds on the dashboard's rails and the session picker; each pane keeps
  its own composer, scroll and status. *Check:* two panes run two turns at
  once; resize and focus by keys and mouse; layout restored on restart.
- **W2. Realtime voice calls with the agent** — talk and be answered as a
  conversation, not turn by turn: speech streamed in (Whisper on rolling
  chunks), the reply spoken as it is written (sentence by sentence through
  the Piper/Windows voice), barge-in (speaking cuts the reply off), and the
  agent's tool work narrated briefly ("reading main.rs…"). Mic and speaker
  pick in `/talk devices`; `/talk call` starts it, Esc ends. Builds on
  `/talk loop` and `/call` (people's calls already stream audio).
  *Check:* first spoken word under ~1.5 s after the reply starts streaming;
  barge-in stops speech within 300 ms; works with the fake audio device in
  tests.

## 5c-fix. Split workspace keys don't work (reported 2026-11-19, v1.0.86)

- **W1-fix (done v1.0.87). Replace Ctrl+X as the split key** — found: Ctrl+X `|` works in a plain PTY (test added), so the user's terminal takes Ctrl+X; kept as a fallback, Alt keys + `/split` added — the user pressed Ctrl+X then
  `|` and nothing happened. Ctrl+X is likely taken before dcode-ai sees it
  (the terminal's or VS Code's "cut", the composer's own Ctrl+X, or the vim
  layer), so the leader never arms. Change to keys that reach the app in
  every terminal, and keep a command that always works:
  - `Alt+|` (or `Alt+\`) split right, `Alt+-` split below, `Alt+O` next
    pane, `Alt+W` close pane, `Alt+A` change what it shows (dashboard only;
    Alt+1–8 are already the tabs);
  - `/split right|below|close|next|show <agent|side>` as the always-works
    way, listed in `/` and Ctrl+P;
  - footer hint in the dashboard while split ("Alt+O next pane · Alt+W close");
  - first find out why Ctrl+X didn't arm (log key events in a PTY test on
    WSL / Windows Terminal / VS Code) so the old leader is either fixed or
    removed, not left half-working; update README, CHANGELOG, `?` help.
  *Check:* PTY test that Alt+| opens a pane and `/split close` closes it.

## 5d. Free entry: local models and model routing (2026-11-19)

Changes an earlier decision: since 2026-10-13 Claude and Copilot are the
only providers (AGENTS.md). Local models come back as a third, opt-in
provider, chosen by the user on 2026-11-19 as the main step to free entry.

- **M1. Local models (Ollama, LM Studio)** — a `local` provider speaking the
  OpenAI-compatible chat API both expose (`http://localhost:11434/v1`,
  `http://localhost:1234/v1`). Found automatically at start (a quick probe
  of both ports); `/models` lists the installed models with size and
  context; `/model local:qwen3-coder` picks one; tool calls through the
  native tool API when the model supports it, else the `<tool_call>` text
  protocol already used for the claude-code backend. Onboarding offers it
  when no Claude or Copilot sign-in exists ("Use a free local model"). No
  downloads by dcode-ai — it points to `ollama pull …`.
  *Check:* mock OpenAI-compatible server in tests (tool calls native and
  text); the empty-completion rule still fails loudly.
- **M2. Automatic model routing** — `model = "auto"`: each turn goes to a
  model by what it needs — quick questions, reads and small edits to a
  cheap/fast one (Haiku, a Copilot mini model, or a local model), planning,
  long edits and hard failures to a strong one (Opus/Sonnet, GPT); a
  failed or empty answer retries one tier up. Rules visible and editable
  (`[model.routing]`), the chosen model shown on each turn, `/cost` shows
  savings. *Check:* routing table tests; a mock run where a failing cheap
  answer escalates.

## 6. Code health (found reading the code)

23. **Split the biggest files** — `jira_sync.rs` 4.6k lines,
    `supervisor/mod.rs` 4.3k, `repl.rs` 3.9k, `tui/state/mod.rs` 3.8k,
    `main.rs` 3.8k, `video/render.rs` 3.4k. Smaller modules rebuild faster
    and are easier to change safely.
24. **Leftover empty crate folder** `crates/web-api` — remove.
25. **Release binary is 55 MB** — speech (candle/Whisper), p2p (iroh),
    video and the browser are always built in. Cargo features for the heavy
    parts (default on, so nothing changes for users) let a lean build and
    CI jobs skip them; try `opt-level = "s"` for size.
26. **Dependency diet** — 820 crates. Find unused dependencies
    (`cargo machete`), duplicate versions (`cargo tree -d`: 3 `base64`,
    3 `getrandom`, 3 `hashbrown`…), and heavy ones used for little.
27. **Startup time** — measure time to first frame; load MCP servers, the
    speech model and the repo map after the first paint.
28. **Panics in parsers** — `autoresearch/metric_parser.rs` has 18
    `unwrap`/`expect` outside tests: return errors instead.

## Order

**Done (v1.0.85, 2026-11-19):** A1 swarm board, A2 workflow fan-out
(declarative `tasks` / `items`+`template`, not a JS script), W1 split
workspace (panes beside the conversation in the dashboard; one composer
routed to the focused pane), TUI 6–9, M1 local models (native tool calls
only — a model without tool support gets a clear error; the `<tool_call>`
text fallback is not built), M2 auto routing (no `/cost` savings figure;
costs are estimated at the strong model's prices). **On hold** (user, 2026-11-19): everything else — TUI 6–9, M1,
M2, W2, video 11–15, talk 16–18, agent 19–22, code health 23, 25–28.

Earlier order: 1 → 2 → 3 → 4 → 5 → 24 → 26 → 6 → 7 → 8 → M1 → M2 → A1 → A2 → W1 → W2 → 11 → 12 → 19 → 23,
the rest as asked.
