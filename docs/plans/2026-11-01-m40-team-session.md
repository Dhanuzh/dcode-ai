# M40 — A team on one session (v1.0.39)

Status: implemented

Picked from the list after the first real `/invite` use.

## 1. Guests reconnect on their own

- On admission the host gives the guest a rejoin token (per guest, valid
  while the invite is open). If the connection drops, the guest retries
  every 2 s for 2 minutes with `hello {name, rejoin}`: no knock, same
  person, same desk.
- The host waits 20 s before saying the guest left, so a short drop shows
  nothing in the session. A rejoin gets no history again (the guest's screen
  still has it); the guest sees "reconnecting…" / "reconnected".
- No reconnect after `/kick`, `/invite stop` or the session ending (the host
  marks those notes `final`).

## 2. `/people` and `/kick <name>`

- `/people`: everyone with the session — you, guests on `/invite` (and
  the browser), local `join`s, `/watch` viewers.
- `/kick <name>`: removes guests on `/invite` with that name (terminal and
  browser): they are told, disconnected, their rejoin token dropped. Local
  `join` and `/watch` can't be kicked from here (`/watch stop` ends the page).

## 3. The host's project on the guest's screen

`admitted` carries the project (workspace folder name). The guest's header
shows it ("d-note · hosted by Dhanush") instead of the guest's own folder;
`@` file completion is off for guests (their files aren't the session's).

## 4. QR code

`/invite web` and `/watch lan` print the link as a QR code too (half-block
characters), for phones.

## 5. Team chat and a shared to-do list

- **Chat** (`/chat <text>`, for host and guests, also the browser page):
  between people, not sent to the agent. Shown as `💬 Priya: …` in the
  transcript, in the speaker's office bubble, and in the dashboard's new
  **team** tab (`Alt+6`).
- **To-dos** (`/todo <text>`, `/todo done <n>`, `/todo` lists): a list the
  whole session shares. Every turn the agent gets the open items (in a
  `<team-todos>` block after the prompt, not shown in the transcript) and is
  told to work on them when they fit and to say when one is done; people
  tick them off. Also in the team tab.
- New commands `Chat`, `TodoAdd`, `TodoDone`; events `PersonChat`,
  `TeamTodoAdded`, `TeamTodoDone` (kept in the log, so `/resume` brings the
  list and the chat back).

## Tests

- Invite: rejoin without knocking keeps the person; kick ends a guest and its
  token; chat and todo commands pass through.
- Runtime: chat events don't reach the agent; todo add/done events.
- TUI state: to-dos and chat from events; the prompt block lists open items;
  the block is hidden in the transcript.
- QR renders for a URL.
- PTY: host and guest chat, guest adds a to-do that reaches the agent's
  prompt, `/people` lists them, `/kick` removes the guest.
