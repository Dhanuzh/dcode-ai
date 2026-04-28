# Premium UI Pass: Transcript Search + Composer History + Clarity

Date: 2026-04-29
Owner: Codex
Scope: `crates/cli/src/tui/app.rs`, `crates/cli/src/tui/state.rs`
Status: Completed

## Goals
- Add transcript search UX (`Ctrl+F`) with clear match feedback and next/prev navigation.
- Add composer message history navigation (Up/Down on empty composer) for fast reuse.
- Improve visual clarity/discoverability for navigation and copy behavior without adding noise.

## Implemented
1. State model updates
- Added transcript-search state fields to `TuiSessionState`.
- Added `open_transcript_search` / `close_transcript_search` helpers.

2. Transcript search render + navigation
- Added case-insensitive transcript matching from rendered lines.
- Added in-transcript match highlighting.
- Added compact transcript-search popup (`Ctrl+F`, Enter/Down next, Up previous, Esc close).
- Added auto-scroll to keep selected match in view while search popup is open.
- Added match index/count in transcript title.

3. Composer history
- Added in-memory composer history for the active TUI session.
- Added Up/Down recall with draft preservation.
- Added bounded history size (200 entries), deduping consecutive identical submissions.

4. Clarity polish
- Updated empty-composer hint to include history and transcript navigation discoverability.
- `click to copy` affordance now appears only when mouse capture is ON.
- When mouse capture is OFF, transcript shows `F12 enables click copy`.
- Added `F6` to copy the latest assistant response without mouse capture conflicts.

## Validation
- `cargo fmt --all`
- `cargo test -p dcode-ai-cli`
- Result: pass

## Notes
- No provider/runtime protocol changes; this pass is CLI/TUI UX only.
