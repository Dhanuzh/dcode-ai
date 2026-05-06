# 2026-05-06 TUI Paragraph Spacing + Session Logo Banner

## Scope
1. Improve assistant readability by adding visible spacing between top-level markdown paragraphs.
2. Show dcode crab-style logo/banner when TUI starts and when user creates a new session (`/new`).

## Implementation Plan
1. `crates/cli/src/tui/app.rs`
- Add a reusable `session_start_banner()` helper returning multiline ASCII crab + dcode label.
- Replace existing single-line startup system message with banner + short hint line.
- In markdown renderer, insert one blank line after top-level paragraph end (not inside lists/quotes).
- Update `DisplayBlock::System` rendering to support multiline content cleanly.

2. `crates/cli/src/repl.rs`
- On `/new`, after clearing transcript state, push `DisplayBlock::System(session_start_banner())` and a hint line.

## Validation
1. `cargo test -p dcode-ai-cli` (or targeted TUI tests if full suite is heavy).
2. Manual TUI check:
- New session shows logo.
- `/new` shows logo again.
- Assistant paragraph blocks have extra vertical gap without breaking list layout.
