# M39 — Guests get the same interface (v1.0.38)

Status: implemented

A guest on `/invite` saw plain text. They now get the host's terminal UI:
the same `TuiSessionState` and `run_blocking` screen, fed from the invite
connection instead of a local agent.

## Host

On admission the host sends, besides `admitted`: its name, the session id,
the model and the guest's person id (`you`), then `history`: the session's
events so far (as `/resume` replays them: no deltas, no people), the newest
3000 / ~6 MB.

## Guest (`tui::guest`)

- History → `replay::apply_history`; live events → `apply_event`; notes →
  system lines.
- Enter → `say`; typed during a busy turn → the local queue is drained to
  `say` (the host queues it). Esc / `/stop` → `stop` (refused unless
  `--let-approve`). Approval verdicts → `approve/deny`. Questions → host
  only.
- Guest commands: `/office`, `/timeline`, `/help`, `/quit`; anything else
  says it isn't available while joined.
- Office: the host's desk "(host)", the guest's own "(you)" (`Office::me`).
- `--plain` (or no terminal) keeps the text mode.
- No sign-in of their own is needed.

## Also

- The office no longer shows dcode-ai's own `<task-notification>` messages
  as what the owner said.
- README: Agent Office screenshot.

## Tests

- Invite unit test: history arrives first, host name and person id.
- PTY: host has a conversation and invites; guest (own home, no sign-in) in
  a terminal sees it, sends a message and sees the reply, `/office` shows
  "(host)" and "Sam (you)", `/model` is refused, `/quit` leaves.
