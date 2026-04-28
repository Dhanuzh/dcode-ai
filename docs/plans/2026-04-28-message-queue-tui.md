# TUI Message Queue Mode (Enter vs Alt+Enter)

## Goal

Add queue-aware message submission in full-screen TUI so users can stage instructions while a turn is busy, matching strong agent-CLI behavior.

## Scope

- `Enter` during busy turn queues a steering message.
- `Alt+Enter` during busy turn queues a follow-up message.
- Steering queue drains before follow-up queue.
- Show queue counts in status bar and queue hints in composer footer.
- Keep IPC/NDJSON payloads unchanged.

## Implementation

- [x] Extend `TuiCmd` with queue variants.
- [x] Add queue counters to `TuiSessionState`.
- [x] Update TUI Enter handling to enqueue commands while busy.
- [x] Add REPL-side queue dispatcher with steering-first drain policy.
- [x] Ensure transcript cache invalidation for REPL-side system lines.
- [x] Run focused CLI tests.

## Validation

- `cargo test -p dcode-ai-cli approval_parse_tests`
- `cargo test -p dcode-ai-cli tui::state::tests`
- `cargo test -p dcode-ai-cli`
