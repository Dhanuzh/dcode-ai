# Plan — sign-in expiry, diagrams, replay, phone approvals, tour, Slack, talk (2026-11-12)

**Status:** built in v1.0.63. Differences from the plan: the optional push
is `[ui] push_url`, the speech model `[ui] stt_model`; `/talk loop` stops on
"stop" (not Esc). Teams added too: an outgoing webhook (HMAC-signed) through
a tunnel the user runs, answers posted with a Workflows webhook.

## 1. Sign-in expiry warning
- Claude (`claude-code` backend): `claude auth status --json` → `loginExpiresAt`.
  Copilot: the stored GitHub token is checked against GitHub (`GET /user`):
  valid / revoked. `ant` (API backend) refreshes its own tokens: not checked.
- On start (once a day, cached in `~/.dcode-ai/signin-check.json`): within 10
  days of expiry, or expired/revoked → a notice with the command to renew.
- `dcode-ai doctor` shows each sign-in and when it expires.
- `/schedule` runs: before launching, an expired sign-in skips the run with a
  notification instead of failing silently.

## 2. Diagram viewer
- Mermaid `flowchart`/`graph` (TD, TB, LR, BT, RL) and `sequenceDiagram`
  drawn as boxes and arrows with box-drawing characters, in Rust (layered
  layout: ranks by longest path, order within ranks by barycenter).
- `/diagram` (and `Ctrl+]`) opens the last reply's diagrams full screen;
  ←/→ between diagrams, arrows/PgUp/PgDn scroll, `c` copies the source, Esc.
- A reply with a mermaid block shows a hint: "diagram — Ctrl+] to view".
- Unsupported diagram kinds: the source, with a note.

## 3. Replay (`/replay`)
- `/replay [session-id] [--speed N] [--gif]` turns a session's event log into
  an asciinema v2 recording (`.dcode-ai/replays/<id>.cast`): prompts typed,
  replies streamed, tool lines, with real timing compressed (gaps ≤ 1.5 s).
- `--gif`: converted with `agg` when installed; otherwise says how.
- `dcode-ai replay <id>` plays it in the terminal.

## 4. Approvals on your phone
- `/invite phone`: the web invite page with approval rights for the person
  who opens it (you), and a QR code. The page shows Allow / Deny cards for
  approvals and questions; while it's open it alerts (sound, vibration,
  title) when the agent needs you.
- Optional push when the page isn't open: `[notify] push_url` (an ntfy-style
  URL you choose) gets "dcode-ai needs approval in <folder>" — no code, no
  command text — when an approval waits more than 30 s and the terminal isn't
  focused.
- Same knock-and-admit flow as other invites; the key is in the link.

## 5. Codebase tour (`/tour [area]`)
- A preset: the agent reads the project (repo map, entry points, tests) and
  gives a guided tour — what it does, the main pieces in reading order with
  `path:line` links, how one request flows through, where tests and config
  live, how to run it — then offers to go deeper. Read-only (plan mode).

## 6. Slack mention
- `dcode-ai slack setup` stores a Slack app's bot token (`xoxb-`) and app
  token (`xapp-`, Socket Mode — no public URL) readable only by you, and the
  Slack user ids allowed to ask (default: yourself).
- `/slack on` (in a session) or `dcode-ai slack serve`: `@dcode-ai <question>`
  in a channel the app is in → a read-only sub-agent (explorer tools, plan
  mode) answers in the thread. Secrets masked. Only allowed users; others get
  "not allowed". Teams: later (it needs a public HTTPS endpoint).

## 7. Talk to the agent (`/talk`)
- Speech to text on your machine with Whisper (candle, pure Rust; model
  `tiny.en` quantized, ~40 MB, downloaded once to `~/.dcode-ai/models/`).
- `/talk`: listens (mic at 16 kHz, stops after 1.5 s of silence or Enter),
  transcribes, and puts the text in the prompt to send or edit;
  `/talk send` sends it; `/talk loop` = hands-free: listen → send → speak
  the reply (`/speak`) → listen again, until "stop" or Esc.
- `/standup voice`: talk through your day; the agent turns it into board
  cards and Jira comments (asks before posting to Jira).
- `[voice] stt_model = "tiny.en" | "base.en"` (base: better, ~290 MB).

## Order
1 → 5 → 3 → 4 → 2 → 7 → 6.
