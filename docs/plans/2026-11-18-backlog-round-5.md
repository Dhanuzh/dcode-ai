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

## 2. TUI

6. Search this conversation from Ctrl+P (`?`), jump to the message.
7. Clickable `path:line` in replies opens the editor (VS Code, JetBrains,
   `$EDITOR`).
8. `/recap` opens the sharp PNG instead of the blurry half-block preview.
9. `/rewind` shows the diff that would come back before confirming.
10. Collapse long tool output by default in the inline view (Ctrl+O shows
    all), like the dashboard.

### UI picks (chosen 2026-11-18)

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

1 → 2 → 3 → 4 → 5 → 24 → 26 → 6 → 7 → 8 → 11 → 12 → 19 → 23, the rest
as asked.
