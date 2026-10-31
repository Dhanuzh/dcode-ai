# M32 — Resume loses the latest messages (v1.0.32)

Status: implemented

## What happened

Seen in a real workspace (ZeO_Release_Jira_Dashboard): sessions with 300+
tool calls had an event log but no saved state; resuming showed only the
first part of a conversation.

1. `Supervisor::resume` built its runtime with `create()` under a fresh,
   throwaway id, then switched to the loaded session's id and deleted the
   throwaway's files. But `create()` had already announced the throwaway id
   (`SessionStarted`) on the event stream. The TUI followed it, and the TUI
   bridge writes events to the log of the TUI's current session id, so the
   whole resumed conversation went into `<throwaway>.events.jsonl`. Its state
   file had been deleted, so these logs looked orphaned.
2. The next resume replayed only the session's own log: everything after
   the earlier resume was missing. Each resume split the history again.
3. The bridge switched logs only after the TUI applied an event, so a new
   session's own `SessionStarted` landed in the previous session's log.
   Replay applied that foreign `SessionStarted` and switched the live
   session (and its log) to the other id.
4. A resumed session whose history was all summary (1.0.27/28) counted as
   "empty" on exit, and its state file was deleted.

## Fixes

- `resume` calls `create()` with the session's own id (it no longer saves
  anything, so the loaded state is safe); no throwaway id, nothing deleted.
- The bridge switches the log on `SessionStarted` before writing it.
- Replay skips `SessionStarted` / `SessionEnded`.
- Repair: replay follows the chain of logs that old resumes created — ids
  named in a `SessionStarted` inside the chain that have a log but no saved
  state — and reads them in order. Nothing is rewritten. In the affected
  workspace one session goes from 12 to 45 user messages on resume.
- A resumed session, or one holding a compaction summary, is never deleted
  as empty.
- Found by the new end-to-end test: with `auto_detect_context_window =
  false` and no window set, the window was 0 (floored to 8k), so the system
  prompt alone triggered compaction on the first message; and a sliding-
  window "compaction" that removed nothing was still reported. Both fixed.

## End-to-end tests (`crates/cli/tests/tui_pty.rs`)

A local mock of the Anthropic streaming API (`ANTHROPIC_AUTH_TOKEN` set to
a dummy token that only reaches the mock) answers every request, and the
real binary runs in a PTY:
- talk → quit → `-c` → talk → quit → `-c` (both turns shown) → `/new` →
  talk → `/resume <id>` → talk → quit → `-r <id>`: every resume shows the
  whole history, each session's words are only in its own log, and no log
  exists without saved state. The test fails with the old resume.
- a `write_file` call: the approval prompt appears, `1` approves, the file
  is written, and `-c` shows the call and the reply.
