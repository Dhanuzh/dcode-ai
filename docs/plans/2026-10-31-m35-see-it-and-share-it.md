# M35 — See it and share it (v1.0.34)

Status: implemented

Two directions the user picked: something people see and feel, and team
sharing.

## Visual

### 1. Live file map (dashboard tab 4, `Alt+4`)

Every file the agent touches this session, as a tree: reads dim, edits
bright with `+/-` counts, the file being touched right now highlighted with
a pulse, and how long ago each was touched. Built from `ToolCallStarted`
events (path-like inputs of file tools, shell `cwd`), so it also rebuilds
on resume. `/files` prints the same list inline.

### 2. Timeline you can scrub (`/timeline`)

A full-screen view of the session as steps: your messages, tool calls
(icon per tool), replies, compactions, sub-agents, with time and tokens.
`←/→` (or `j/k`) scrub; the right pane shows that step (input, output
excerpt, files changed by then). On one of your messages, `Enter` opens
rewind at that point. Built from the TUI's event stream as it arrives (and
from the replayed log on resume).

### 3. Sound and voice

- `[ui] sounds = true` (or `/sounds on`): the terminal bell when a long
  turn (≥ 10s) finishes and when dcode-ai needs an approval or an answer.
- `/speak`: reads the last reply aloud with the system's voice (`say` on
  macOS, Windows' speech synthesizer on Windows and WSL, `spd-say` /
  `espeak` on Linux). Code blocks are skipped.

## Team

### 4. Live watch link (`/watch`)

`/watch` starts a read-only web page for the current session:
`http://127.0.0.1:<port>/?t=<token>`. It shows the conversation so far and
then streams new events live (server-sent events from the session's IPC
socket). Read-only: there is no route that sends anything to the session.
- Local by default. `/watch lan` binds the LAN address so a teammate on the
  same network can open it; it says so plainly. Nothing goes through an
  outside service.
- The token is random per watch; `/watch stop` ends it; it ends with the
  session.

### 5. Several people on one session (`dcode-ai join <id>`)

From another terminal (or over `dcode-ai remote` / SSH), `join` shows the
running session live and lets that person type: lines are sent to the
session as messages (queued like Tab in the TUI), `/approve` and `/deny`
answer a pending approval. Uses the session's IPC socket, so only users who
can reach it (same machine, or SSH) can join.

### 6. Handing work to a teammate (`/handoff [note]`)

Writes `<id>.dcode-session` (secrets masked, as `session export`) with a
handoff note: who, when, the note, the open todo list, and the changed
files. `session import` shows the note at the top of the resumed
transcript and gives it to the agent, so the teammate can just say
"continue".

## Tests

- State: file map tracks reads/edits per path; timeline records steps.
- Watch server: page and events need the token; no write routes; history
  then live events.
- Join: messages reach the session over IPC (runtime test).
- Handoff: bundle carries the note; import shows it and adds it for the
  agent.
- PTY: `/watch` prints a link, `/files` and `Alt+4` show touched files,
  `/timeline` opens and scrubs.
