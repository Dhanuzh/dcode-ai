# TUI cancel + paste behavior fixes (2026-04-21)

## Problems reported
1. Repeated cancel spam (`Cancelling current run...`) when pressing/holding cancel keys.
2. Cancel feels delayed instead of becoming a single in-flight action immediately.
3. Multi-line paste into composer behaves poorly; user wants a compact placeholder like `[pasted N lines]`.

## Plan

### 1) Make cancel idempotent from TUI keyboard path
- In `crates/cli/src/tui/app.rs`, add a small helper for cancel requests that:
  - uses `cancel_flag.swap(true, SeqCst)` to detect first request,
  - emits one `DisplayBlock::System("Cancelling current run...")` only on first request,
  - sends `TuiCmd::CancelTurn` only once per active turn.
- Reuse this helper in Esc and Ctrl+C handlers.

### 2) Improve perceived immediate stop behavior
- When first cancel request is made, set local busy state to idle so follow-up Esc/Ctrl+C doesn’t keep triggering cancel UI noise.
- Final cancellation outcome remains authoritative from runtime events (`run cancelled`).

### 3) Add explicit paste event handling
- Handle `Event::Paste(String)` in TUI event loop.
- Behavior:
  - If pasted payload contains multiple lines, insert token `[pasted N lines]` at cursor.
  - If single-line, insert text directly at cursor.
- Preserve slash/@ completion index clamping after insert.

### 4) Validation
- `cargo fmt`
- `cargo test -p dcode-ai-cli tui::app --no-run`
- Add/adjust unit tests for:
  - idempotent cancel helper,
  - multiline paste token generation.
