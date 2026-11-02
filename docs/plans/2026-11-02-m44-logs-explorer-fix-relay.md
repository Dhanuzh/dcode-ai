# M44 — Smaller logs, project explorer, Alt+F fix, relay (v1.0.48)

Status: implemented

## 1. Smaller session logs

- Live-only events are never written to `<id>.events.jsonl`: streamed text
  and thinking pieces, tool output pieces, heartbeats
  (`AgentEvent::is_live_only`). Resume never used them, since the final
  message and tool result hold the same text. In real logs they were ~75% of
  the lines.
- **Tidy on open**: resuming a session whose log (or its old split-off logs)
  is over 1 MB drops those lines in place (written to a temp file, then
  renamed). It's skipped while another dcode-ai has the session open (its
  IPC endpoint answers), so nothing is lost mid-write.
- `dcode-ai sessions --tidy` tidies every log in the workspace and says how
  much it saved.

## 2. Project explorer in the dashboard

The **files** tab becomes a project explorer: the whole project tree,
respecting `.gitignore`, with what the agent touched marked on it (`✎ 4✎`,
`· 2r`, `◉` now).

- Folders open and close: `↑/↓` move, `→`/`Enter` open, `←` close.
- `Enter` on a file puts `@path` in the composer. `t` switches to "touched
  only" (the old view).
- Click a row to select it; click a folder to open or close it.
- Scanned in the background (capped at 20,000 entries) and refreshed when the
  agent edits something.
- Focus: `Alt+4` then `Ctrl+D` (as for the changes tab), or click.

## 3. "Alt+F to fix" from any terminal

`dcode-ai hook zsh|bash|fish|powershell` prints a snippet for your shell's
rc file (`eval "$(dcode-ai hook zsh)"`). After a command fails it prints
`dcode-ai: Alt+F to fix`; Alt+F runs
`dcode-ai fix --cmd '<the command>' --exit <code>` in that folder. This
opens dcode-ai with a prompt to find out why it failed and fix it,
re-running the command if it's safe. dcode-ai never re-runs anything by
itself. `DCODE_FIX_KEY` changes the key (Alt+F is forward-word in emacs
mode).

## 4. Relay mode

For people on different networks without Tailscale.

- `dcode-ai relay serve [--port 8790]` runs a tiny relay on a machine both
  sides can reach (a small VPS). It pairs connections and copies bytes.
- Host: `/invite relay <host:port>` (or `[invite] relay = "host:port"` in
  the config). The line becomes `dcode-ai join ROOM@relayhost:8790/KEY`.
- Protocol, line-based before the bytes flow:
  - host control link: `HOST <room> <secret>`; the relay answers `OK`, then
    sends `CONN <n>` for each guest;
  - the host opens `PAIR <room> <n> <secret>` per guest, and the relay
    splices it to the waiting guest;
  - guest: `GUEST <room>`; the relay answers `OK` when paired. Then the
    usual `DCAI1` + Noise handshake runs end to end.
- **The relay can't read or change anything.** The invite key never reaches
  it; it only sees Noise ciphertext. The room id is random and the secret
  stops anyone else claiming the room.
- Limits: 64 rooms, 16 guests per room, 30 s to pair, idle links closed
  after 2 minutes without a host.
- Calls, chat and everything else work through it unchanged (same
  connection).

## Tests

- Live-only events aren't written; tidy keeps everything else and shrinks a
  log; tidy skips a session that's open.
- Explorer: tree from a temp project (gitignore respected), touched marks,
  expand/collapse, `t` toggle; PTY: `Alt+4` shows project files.
- Hook: snippets for each shell; `fix` builds the prompt.
- Relay: in-process relay; a host and a guest on different "networks"
  (only the relay port reachable) join through it, the relay sees no
  plaintext, and a wrong room is refused.
