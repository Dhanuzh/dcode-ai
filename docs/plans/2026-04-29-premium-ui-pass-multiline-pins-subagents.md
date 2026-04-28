# Premium UI Pass 2: Multiline + Pins + Sub-agent Focus

Date: 2026-04-29
Owner: Codex
Scope: `crates/cli/src/tui/app.rs`, `crates/cli/src/tui/state.rs`, `crates/cli/src/repl.rs`
Status: Completed

## Goals
- Add true multiline composer behavior with `Shift+Enter`.
- Add pinning workflow so important content stays visible even when transcript is cleared.
- Add sub-agent detail modal with quick session focus and visible progress.

## Implemented
1. Multiline composer
- `Shift+Enter` now inserts a newline at cursor (never queues/sends).
- Composer height calculation now accounts for explicit newline splits plus wrapping.

2. Pins workflow
- Added pinned-note state, top-of-transcript pinned section, and quick actions.
- `Ctrl+K`: pin latest assistant/user/tool summary (dedup + cap 20).
- `Ctrl+J`: open pins modal with select, remove, copy selected, and jump-to-top behavior.
- Pins are independent from transcript blocks, so they persist when screen/transcript clears.

3. Sub-agent dashboard
- `Ctrl+G`: open sub-agent modal with status rows and progress bars.
- Enter on selected row focuses that sub-agent session via `ResumeSession`.

4. Discoverability / polish
- Updated inline TUI hints and first-run helper line for new shortcuts.
- Updated `/help` keyboard section in REPL with new TUI shortcuts.
- Mouse-capture OFF message now points to `F6` fallback copy.

## Validation
- `cargo fmt --all`
- `cargo test -p dcode-ai-cli`
- Result: pass

## Notes
- Changes are TUI/CLI UX only; no runtime/provider protocol changes.
