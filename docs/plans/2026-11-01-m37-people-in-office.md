# M37 — People in the Office (v1.0.36)

Status: implemented

When a teammate or a friend is on a session, they should show up in the
Agent Office too, with their name.

## Who is in the room

- **You** (the one running the session): first desk, notebook, name tag in
  the accent colour, "(you)". Name from git `user.name`, else the login.
- **Joined** (`dcode-ai join <id> [--name]`): a guest desk with a mug, blue
  name tag. Can send messages.
- **Watching** (the `/watch` page, optional name): standing at the side with
  a mug, grey name tag. Read-only.

What someone says shows in their bubble for 8 s. Someone not heard from for
25 s (no heartbeat) gets up and leaves; leaving is a wave and a walk out.

## How presence travels

- New IPC commands: `Hello {person, name}`, `Heartbeat {person}` (every
  10 s), `Bye {person}`, `Say {person, content}`.
- The runtime's command consumer turns them into session events
  `PersonJoined {person, name, role}`, `PersonSeen` (not written to the log),
  `PersonLeft {person, name}`, `PersonSaid {person, name, text}`; a `Say`
  also goes to the agent as `(from Name) …`.
- The `/watch` server emits the same events for each open `/events` stream
  (`role: "watch"`, `PersonSeen` every ~5 s, `PersonLeft` when the page
  closes). Who was here earlier is not replayed to a new page.
- Names are cleaned (letters, digits, spaces, `-_.`, ≤ 24 chars, else
  "guest") and display-only; the page escapes everything.
- Resuming a session does not replay people (they are not here now).

## Tests

- Runtime: Hello/Say/Bye → events, the agent gets `(from Name)`.
- Office: join, talk, leave; order owner → agents → joined → watching.
- Watch: name decoding; a watcher arrives and leaves with the page; history
  doesn't bring back old people.
- PTY: two people join with names, one watcher; a message reaches the agent
  with the name; all three are in `/office`; all three leave.
