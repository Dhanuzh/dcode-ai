# M7a — Claude-style terminal UX, `/rewind` with code, plan → build

Date: 2026-10-14 · Branch: `feat/claude-copilot` · Status: ✅ done, live-tested (Copilot gpt-5-mini) · Parent: [v1 master plan](2026-10-12-v1-master-plan.md) (M7)

The user asked for the pending M7 items with a UI/UX "like Claude", meaning the
Claude Code terminal look and interaction model. dcode-ai keeps its own name,
colours tokens and features; only the interaction grammar and visual language
follow Claude Code. Nothing here copies Claude Code assets or code.

## Target look (inline view)

```
╭─────────────────────────────────────────────╮
│ ✻ Welcome to dcode-ai!                      │
│                                             │
│   /help for help · /model to switch model   │
│                                             │
│   cwd: ~/project/d-folio                    │
│   model: claude-sonnet-5 · GitHub Copilot   │
╰─────────────────────────────────────────────╯

> read a.txt and rename line 11

⏺ I'll read the file first.

⏺ Read(a.txt)
  ⎿  Read 30 lines (ctrl+o to expand)

⏺ Update(a.txt)
  ⎿  Updated a.txt with 1 addition and 1 removal
       10  line TEN
       11 -line 11
       11 +line ELEVEN
       12  line 12

⏺ Update Todos
  ⎿  ☒ Read the file
     ☐ Rename the line

⏺ Done — line 11 now reads "line ELEVEN".

✻ Pondering… (12s · ↓ 1.2k tokens · esc to interrupt)

╭─────────────────────────────────────────────╮
│ > _                                         │
╰─────────────────────────────────────────────╯
  ⏵⏵ accept edits on (shift+tab to cycle)        claude-sonnet-5 · 12% ctx
```

## Scope

### A. Transcript grammar (`tui/transcript.rs`)
- User: `> text`, muted, no full-width bar; wrapped lines indent 2.
- Assistant: `⏺ ` first line, 2-space continuation.
- Tools: `⏺ Name(arg)` — dot green ok / red error / muted while running;
  Claude-style names (Read, Write, Update, Bash, Search, List, Fetch, Task,
  Update Todos …). Result line `  ⎿  summary` (dim): Read → `Read N lines`,
  edits → `Updated p with N additions and M removals` + diff,
  errors → red `Error: …`, others → first output lines.
- `update_plan` → `⏺ Update Todos` with `☒` (done, dim strikethrough),
  `☐` (pending), bold `☐` (in progress).
- Thinking: `✻ Thinking…` dim italic, collapsed preview (ctrl+t expands).
- No turn separator rules; one blank line between items.

### B. Chrome (`app.rs` banner, `app/frame.rs`)
- Welcome box: rounded, accent border, `✻ Welcome to dcode-ai!`, tips, cwd,
  model · provider.
- Spinner row above the composer while busy: glyph cycles `· ✢ ✳ ✶ ✻ ✽`
  (accent), a verb picked per turn (Thinking, Pondering, Crafting …),
  `(Ns · ↓ N tokens · esc to interrupt)`.
- Composer: rounded muted border, `> ` prompt, placeholder
  `Try "explain this codebase"`.
- Footer (replaces the status bar): left = mode indicator
  (`⏵⏵ accept edits on`, `⏸ plan mode on`, `⏵⏵ bypass permissions on`, each
  `(shift+tab to cycle)`) or `? for shortcuts`; right = model · context %.
- **Shift+Tab** cycles Default → Accept edits → Plan → Default
  (`TuiCmd::CyclePermissionMode`).
- `permissions.startup_approve_all` defaults to **false** (Claude Code has no
  start-up "approve everything" question).

### C. Permission prompt (`render/modals.rs::render_approval_popup`)
Bottom-anchored, full width, Claude grammar:
```
╭ Edit file ──────────────────────────────────╮
│  a.txt                                      │
│   11 -line 11                               │
│   11 +line ELEVEN                           │
│ Do you want to make this edit to a.txt?     │
│ ❯ 1. Yes                                    │
│   2. Yes, allow all edits this session      │
│   3. No, and tell dcode-ai what to do (esc) │
╰─────────────────────────────────────────────╯
```
Bash: title `Bash command`, the command, `Do you want to proceed?`.
Keys: `1/2/3`, `y/a/n`, ↑↓ Enter, Esc = No; `h` per-hunk staging kept.

### D. Plan → build
- Shift+Tab into plan mode (or `/plan` with no args).
- When a plan-mode turn finishes with an assistant reply, show
  `Ready to code?` with `1. Yes, and auto-accept edits`,
  `2. Yes, and manually approve edits`, `3. No, keep planning`.
  1/2 → set permission mode (AcceptEdits / Default) and submit
  "The plan is approved — implement it now."; 3 → back to the composer.

### E. `/rewind` with code (Esc Esc)
- Core: each undo entry records the history position of the user message
  that started its turn; `abort_turn` keeps the snapshot if files changed
  (an errored/cancelled turn's edits are still rewindable). Compaction /
  history replacement marks positions unknown (still `/undo`-able, not
  rewindable).
- Runtime: `rewind_files_to(pos)` restores, newest first, every entry at or
  after `pos`.
- TUI: Esc Esc (idle, empty composer) or `/rewind` opens the message picker;
  after picking, choose `1. Restore code and conversation`,
  `2. Restore conversation`, `3. Restore code`, `Esc` cancel.

## Out of scope
Dashboard rails keep their layout (they reuse the same transcript renderer,
so they pick up A automatically). Themes other than the default keep working
through the existing `theme::*` tokens.

## Verification
Unit tests for: transcript grammar (snapshot), tool summaries, todo rendering,
mode cycle, plan-exit prompt state, rewind restore (core + runtime), approval
prompt rendering (snapshot). Live tmux run against Copilot and Claude.
