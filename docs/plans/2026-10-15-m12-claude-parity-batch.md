# M12 — Claude parity batch: Ctrl+B, thinking keywords, output styles, cleanup

Date: 2026-10-15 · Branch: `main` · Status: ✅ done (v1.0.4) · Parent: [v1 master plan](2026-10-12-v1-master-plan.md)

Request: "continuously improve dcode-ai, I want all Claude features, one by
one, then release". Picked the Claude Code features dcode-ai still lacked
that people use daily.

1. **Ctrl+B — background a running command.** The interactive session's
   `execute_bash` runs the command as a task and races it against its
   timeout and a process-wide "move to background" signal. Ctrl+B (when a
   shell command is running) fires the signal; the task is adopted by the
   session's background jobs (`BackgroundJobs::adopt`), so it gets the
   footer badge, `/bashes`, the `●` end line and the agent wake-up from
   M11. Sub-agents never listen. A cancelled turn still kills the command.
2. **Thinking keywords.** `think hard` → high, `think harder` /
   `megathink` → xhigh, `ultrathink` → max effort for that message only
   (only ever raised, restored after the turn); keywords are drawn in
   rainbow letters in the composer.
3. **Output styles.** `/output-style`: Default, Explanatory, Learning and
   custom `output-styles/*.md` (frontmatter `name`, `description`) from
   `.dcode-ai/` or `.claude/`, project then home. Saved in
   `[ui] output_style`; added to the system prompt.
4. **Session cleanup.** `[session] cleanup_period_days` (default 30, like
   Claude Code's `cleanupPeriodDays`; 0 keeps everything) prunes old
   sessions — state, event log, attachments — in the background at
   startup; live sessions are kept. `dcode-ai sessions --prune <DAYS>`.

Also: the bypass-mode system prompt no longer says shell "asks once".

## Verification
Unit: Ctrl+B adoption with exit code and output, keyword → effort,
keyword ranges, style discovery/parsing/prompt, pruning (old vs fresh vs
live, attachments removed). Live (tmux, Claude): Ctrl+B on `sleep 20` →
backgrounded, turn ended, follow-up turn on completion; `/output-style`
list, switch, reset.
