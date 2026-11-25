# Terminal pane (2026-11-25)

Asked by the user: "can we open a side terminal, like a normal terminal …
in the dashboard; in normal view is it possible?"

## What

A real shell beside the conversation, in the dashboard's split workspace —
a fourth pane kind next to sub-agent and side-chat panes.

- `/term` (alias `/terminal`, `/split term`) or **Alt+T** opens it. From
  the inline view it switches to the dashboard first: the inline view
  writes the conversation into the terminal's own scrollback at full
  width, so nothing can sit beside it there. Ctrl+\ goes back to inline;
  the shell keeps running and is there again on return.
- Runs `$SHELL` (else `/bin/bash`, `sh`; `%COMSPEC%`/PowerShell on Windows)
  in a pseudo-terminal (`portable-pty`, ConPTY on Windows) in the workspace
  folder, `TERM=xterm-256color`. The screen is kept by `vt100` (already a
  dependency for `dcode-ai host`) with 2,000 lines of scrollback, drawn
  cell by cell with colours, bold/italic/underline/inverse and the cursor.
- Focused (Alt+O to it, or click it) it gets every key — Ctrl+C, Tab,
  arrows in the program's cursor mode, F-keys, Alt+letters, bracketed
  paste — except the workspace keys: Alt+O / Alt+W / Alt+A / Alt+\ / Alt+-
  and Ctrl+\. Shift+PgUp/PgDn scroll its history.
- Resizes with the pane. Output wakes the screen (fast ticks while a
  terminal pane is open).
- Alt+W on it closes the pane and ends the shell. If the shell exits the
  pane says so; Enter starts a new one.
- Not restored on restart (it would start a shell nobody asked for).

## Check

- Unit: key → bytes (Ctrl+C = 0x03, arrows normal and application mode,
  Alt prefix, Enter = `\r`), screen → buffer cells (colour, inverse, cursor).
- Unit (unix): spawn `sh`, write `echo hi`, the screen shows `hi`.
- Live in tmux: `/term`, run `ls`, Alt+O back to the conversation.
