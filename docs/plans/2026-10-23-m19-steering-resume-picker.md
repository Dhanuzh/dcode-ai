# M19 — messages during a turn, `/resume` picker (v1.0.18)

Status: implemented

References: Claude Code (messages typed while it works reach the model at
its next step; "↑ to edit queued messages"; Esc puts them back; `/resume`
list replaces the prompt with title + `time · messages · branch` rows) and
Codex (Enter steers the running turn, Tab queues for after it).

## 1. Messages during a turn

Before: Enter / Alt+Enter while busy both waited for the turn to end, the
queue was never drawn, and the "N queued" footer hid behind the mode label.

- `AgentLoop` gets a shared `SteerInbox` (`Arc<Mutex<Vec<String>>>`). Before
  each model request after the first, and when a reply would end the turn,
  the agent adds waiting messages as one user message
  (`<user-message>…</user-message>`, "take it into account now and
  continue") and emits `MessageReceived{user}` so the transcript shows it
  where the model saw it.
- TUI keys while busy: **Enter** → inbox (send now); **Tab** /
  **Alt+Enter** → follow-up queue (one turn each after this one); `/…`,
  `!…`, `#…` always queue. **↑** on an empty prompt takes every unsent
  message back to edit; **Esc** (cancel) puts them back in the prompt too.
- Unsent messages are listed above the composer: `↳ text · next step` /
  `↳ text · after this turn`, plus `↑ to edit`. The footer shows
  `enter send now · tab queue for after` while you type during a turn.
- Inbox messages the agent did not reach (sent as it finished) go out as the
  next turn.

## 2. `/resume` picker

Before: a small centred popup that, in the inline view, landed over the
footer with one cramped `when · title · model` line per session.

- Drawn in the composer's place, full width: title, search line, two rows
  per session (title; `3h ago · model · branch · 12.4k tokens`), hint with
  position (`2 of 9`).
- Leaves out the current session and sessions that never got a message;
  "No earlier sessions in this folder to resume." when none are left.
- Snapshots load concurrently (`load_snapshots`).
