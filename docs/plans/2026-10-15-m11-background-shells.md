# M11 — Background shells like Claude Code

Date: 2026-10-15 · Branch: `main` · Status: ✅ done (live-tested with Claude: start, footer badge, `/bashes`, automatic follow-up turn) · Parent: [v1 master plan](2026-10-12-v1-master-plan.md)

Request: "like this shell running I want in dcode-ai" — Claude Code shows a
long command running in the background, keeps working, lists it in the
footer (`1 shell`), ends the turn with `✻ Crunched for 1m 9s · 1 shell still
running`, and wakes the agent up when the command finishes.

## Today
`run_background` (start / list / output / stop) exists, but nothing in the
UI knows about it and the agent only learns a job ended if it polls.

## Changes
1. **Job events.** `run_background` emits `AgentEvent::BackgroundJob`
   (`job_id`, `command`, `status` running | completed | failed | stopped,
   `exit_code`, `output_tail`) on start and on exit. The supervisor wires the
   tool to the session's event channel, like the bash tool. Exit codes are
   captured.
2. **Tool row.** Starting a job reads
   `Running in the background (job 1 · /bashes to manage)`.
3. **Footer.** `· N shell(s)` while jobs run.
4. **Turn summary.** After each turn longer than 5s (or while shells run):
   `✻ Crunched for 1m 9s · 1 shell still running` (Claude's past-tense verbs).
5. **Wake-up.** When a job finishes, a notification is queued; as soon as
   the agent is idle it receives
   `<task-notification> … </task-notification>` (status, exit code, last
   output lines) as an automatic turn, shown as
   `● Background command "…" completed (exit code 0)`. While busy it waits
   for the turn to end.
6. **`/bashes`.** Lists the agent's jobs with status and output tail;
   `/bashes kill <id>` stops one.

## Verification
Unit: event emission with exit code, notification text, footer badge,
turn-summary formatting. Live (tmux): ask the agent to run `sleep 5; echo
hi` in the background, see the footer badge and the automatic follow-up
turn when it ends.

## Also changed
- `bypass-permissions` no longer asks once for shell commands (Claude Code's
  bypass never prompts; `dcode-ai ci --allow-shell` was refusing the first
  shell command). Destructive shapes and `deny` / `ask` rules still apply.
- The tool header shows the background command (`Background(cargo test)`)
  instead of the action name.
