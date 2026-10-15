# Compact preview plan

Date: 2026-06-03

## Goal

Add `/compact --preview` so users can inspect the preserved context that would
survive compaction before any session summary or memory note is rewritten.

## Current state

- `runtime::Supervisor::compact_summary()` returns a short parent summary of
  recent conversation.
- Automatic compaction already uses a richer fallback summary that includes a
  `Preserved Artifacts` block for files, commands, and errors.
- That richer builder is private to `supervisor.rs` and not reachable from REPL
  or TUI commands.

## Scope

- Add a `Supervisor::compaction_preview()` method that uses the same
  `ContextManager::get_messages_to_summarize()` selection as automatic
  compaction.
- Fall back to all live messages when no summarize range is currently selected,
  so preview remains useful before the threshold is reached.
- Add `SessionRuntime::compaction_preview()` as the CLI-facing wrapper.
- Wire `/compact --preview` in REPL/TUI command handling.
- Keep `/compact` existing behavior unchanged.

## Validation

- Add runtime tests that preview includes preserved artifacts and has a default
  message for empty sessions.
- Add command tests around slash parsing where practical.
