# M27 — Auto mode, paste, compaction, dashboard copy (v1.0.27)

Status: implemented

## Auto mode asked for edits

`resume_in_process` / `reroot_in_process` built a fresh runtime from the
config, so the permission mode fell back to the config default while the
TUI footer still showed auto. Both now carry the live mode (and background
sub-agent setting) into the new runtime. Sub-agents already follow the
parent's mode cell.

## Copilot premium requests

Copilot bills a premium request per user-initiated call. Requests now send
`X-Initiator`: `user` when the last message is a typed prompt, `agent` when
it follows tool output (including the screenshot message attached after a
browser step).

## Automatic compaction

- The agent records the prompt size the provider reports (input + cache
  reads + cache writes); `context_tokens()` is the larger of that and the
  estimate. The supervisor's pre/post-turn checks use it
  (`ContextManager::stats_measured`), as does the footer gauge.
- Mid-turn: `AgentLoop::compactor` (built per turn by the supervisor)
  summarizes older history with the utility model once the prompt passes
  max(threshold, 85%) of the window, before the next model call. Failure
  falls back to the extractive summary; tool-call groups stay intact.
- The "consider summarizing" warning only shows when no compaction runs.

## Paste without bracketed paste

`tui/app/paste_burst.rs`: after reading a plain key, if more keys are
already queued, read the burst; a burst with an inner line break (or 16+
keys with any) becomes one `Event::Paste`. Otherwise the keys are replayed
one by one, so fast typing + Enter still submits.

## Clipboard images on Windows / WSL

One PowerShell helper for native Windows and WSL; WSL tries the Windows
clipboard first (WSLg may hold an older image). When the clipboard holds
image files (Explorer copy), the first one is imported.

## Dashboard

- Drag-select in the transcript pane with highlight; release copies.
- Drag a rail's inner edge to resize it (min 16 columns, centre ≥ 40).
